use std::{fs, io, path::Path, process::exit};

use hound::WavReader;
use ignore::{DirEntry, WalkBuilder};

fn main() {
    let (db, is_overwrite, is_delete_empty) = process_args();
    println!("RUNNING WITH SETTINGS:\n\tminimum db = {}, overwrite input files = {}, delete empty files = {}", db, is_overwrite, is_delete_empty);

    let processor = WavProcessor::new(db_to_normalized_value(db), is_delete_empty, is_overwrite);

    for result in WalkBuilder::new("./")
        .add_custom_ignore_filename(".wavignore")
        .git_ignore(false)
        .ignore(false)
        .git_global(false)
        .git_exclude(false)
        .require_git(false)
        .hidden(false)
        .build()
    {
        match result {
            Ok(entry) => {
                processor.check_file_for_wav(entry);
            },
            Err(err) => println!("ERROR: {}", err),
        }
    }
    println!("Process Finished!");
}

struct WavProcessor {
    deviation_normalized: f32,
    delete_empty: bool,
    overwrite_input: bool,
}

impl WavProcessor {
    fn new(deviation: f32, should_delete_empty: bool, should_overwrite_input: bool) -> Self {
        WavProcessor{deviation_normalized: deviation, delete_empty: should_delete_empty, overwrite_input: should_overwrite_input}
    }




    fn process_wav<T, R>(&self, path: &Path, reader: &mut WavReader<R>, deviation: T)
where 
        T: hound::Sample + PartialOrd<T> + std::ops::Neg<Output = T> + Copy + Default + Ord + std::fmt::Debug,
        R: io::Read
    {
        let samples: Vec<T> = reader.samples::<T>().map(|s| s.unwrap()).collect();
        //
        // Create a vector for each channel to store its samples
        let num_channels = reader.spec().channels as usize;
        let mut channels: Vec<Vec<T>> = vec![Vec::new(); num_channels];

        for channel_idx in 0..num_channels {
            let channel_samples: Vec<T> = samples.clone()
                .into_iter()
                .enumerate()
                .filter(|(i, _sample)| i % num_channels == channel_idx)
                .map(|(_i, sample)| sample)
                .collect();

            // Store the un-interleaved samples into the respective channel's vector
            channels[channel_idx] = channel_samples;
        }

        let mut non_zeroes = Vec::<usize>::new();
        non_zeroes.reserve(channels.len());

        for channel in &channels {
            let mut _last_non_zero = 0;
            // let max_num = channel.iter().max();
            // let min_num = channel.iter().min();

            for (i, sample) in channel.iter().enumerate() {
                if *sample > deviation || *sample < -deviation {
                    _last_non_zero = i;
                }
            }
            non_zeroes.push(_last_non_zero);
            // println!("\n\t[CHANNEL INFO]\nchannel size: {} samples\nlast non zero: {}\nmax: {:?}\nmin: {:?}\nfinal length: {:?}%\n", channel.len(), _last_non_zero, max_num, min_num, _last_non_zero as f32 / channel.len() as f32 * 100f32);
        }


        // keep only channels which aren't empty.
        let mut new_channels: Vec<Vec<T>> = Vec::with_capacity(num_channels);
        for (i, non_zero) in non_zeroes.iter().enumerate() {
            if *non_zero == 0 {
                continue;
            }

            new_channels.push(channels[i].clone());
        }

        // make channels shorter (maximum non zero index)
        let max_non_zero = non_zeroes.iter().max().unwrap();
        for channel in new_channels.iter_mut() {
            channel.truncate(*max_non_zero+1);
        }

        // save new wav
        if let Err(msg) = self.save_new_wav::<T>(&new_channels, &mut reader.spec(), path) {
            println!("{msg}");
        }
    }






    /// saves channel data into the path that was passed in.
    fn save_new_wav<T>(&self, channels: &Vec<Vec<T>>, spec: &mut hound::WavSpec, path: &Path) -> Result<(), String>
where 
        T: hound::Sample + PartialOrd<T> + std::ops::Neg<Output = T> + Copy + Default + Ord + std::fmt::Debug,
    {
        spec.channels = channels.len() as u16;
        let samples_per_channel = {if channels.len() > 0 {channels[0].len()} else {0}};
        if samples_per_channel == 0 {
            if self.delete_empty {

                println!("deleting file because it's empty: {:?}", path);
                // TODO: delete it
                if let Err(e) = fs::remove_file(path) {
                    println!("Couldn't remove file: {:?}\nerr: {}", path, e);
                    return Err(e.to_string());
                }
            }
            return Ok(());
        }

        // add samples interweaved
        let mut write_buf: Vec<&T> = Vec::with_capacity(samples_per_channel * channels.len());
        for sample in 0..samples_per_channel {
            for channel in channels.iter() {
                write_buf.push(&channel[sample]);
            }
        }

        // write new buffer
        let path = {
            if !self.overwrite_input {
                let name: &str = match path.file_name() {
                    Some(name) => name.to_str().unwrap().strip_suffix(".wav").unwrap(),
                    _ => "default_name"
                };
                path.with_file_name(format!("{name}_stripped.wav"))
            } else {
                path.to_path_buf()
            }
        };

        let writer = hound::WavWriter::create(&path, *spec);
        match writer {
            Ok(mut writer) =>
            {
                for sample in write_buf.iter() {
                    // println!("channel export path: {:?}", path);
                    writer.write_sample(**sample).unwrap();
                }
                return Ok(());
            },
            Err(e) => {
                return Err(format!("couldn't open writer\n{e}\npath: {:?}", path));
            }
        } 
    }





    /// finds which bit int was used and processes
    fn setup_wav_processing(&self, path: &Path){
        println!("Processing wav file: {:?}", path.display());
        if let Ok(mut reader) = WavReader::open(path) {
            let bits = reader.spec().bits_per_sample;
            match reader.spec().sample_format {
                hound::SampleFormat::Int => {
                    match bits {
                        16 => {
                            self.process_wav::<i16, _>(path, &mut reader, (i16::MAX as f32 * self.deviation_normalized) as i16);
                        },
                        24 => {
                            self.process_wav::<i32, _>(path, &mut reader, (int_bit_to_max(24, true) as f32 * self.deviation_normalized) as i32);
                        },
                        32 => {
                            self.process_wav::<i32, _>(path, &mut reader, (i32::MAX as f32 * self.deviation_normalized) as i32);
                        },
                        _ => {
                            println!("{bits} bit integer samples not supported!");
                        }
                    }
                },
                hound::SampleFormat::Float => {
                    match bits {
                        _ => {
                            println!("{bits} bit floating point samples not supported!");
                        }
                    }
                }
            }
        }
    }





    /// checks if the current dir or file is a .wav file and processes.
    fn check_file_for_wav(&self, entry: DirEntry) {
        // println!("looking at path: {}", entry.path().display());
        if let Some(file_type) = entry.file_type() {
            if file_type.is_file() {
                // println!("file name: {:?}", entry.file_name());
                if let Some(name) = entry.file_name().to_str() {
                    let name = name.to_lowercase();
                    let name: Vec<&str> = name.split('.').rev().collect();
                    if let Some(extention) = name.first() {
                        if *extention == "wav" {
                            self.setup_wav_processing(entry.path())
                        }
                    }
                }
            }
        }
    }

}

/// Returns (db, is_overwrite, should_delete_empty)
fn process_args() -> (f32, bool, bool) {
    let help_arg = String::from("-h");
    let db_arg = String::from("-db=");
    let overwrite_arg = String::from("-o");
    let delete_empty_arg = String::from("-rm");

    let mut db = -60.0;
    let mut should_overwrite = false;
    let mut delete_empty = false;

    // let mut args_iter = std::env::args().into_iter();

    if let Some(_) = std::env::args().into_iter().find(|b| *b == help_arg) {
        println!("\t[USAGE]");
        println!("Looks for a \".wavignore\" file in the current directory which uses the gitignore style (you can put them in subdirectories too)\n\nFinds all .wav files and trims the end silence off.\nIt will also try to cut whole channels if they are empty.");
        println!("\n\n\t[EXAMPLE]");
        println!("wav_optimizer.exe -db=-55.7 -o -rm");
        println!("\n\n\t[OPTIONS]\n-db=\t\tset a float value for the minimum dB the sample should be at the end when trimming. If not specified, it defaults to -60 dB\n\n-o\t\tif specified in the args, will overwrite the input files with the trimmed version. Otherwise it will add a suffix to the name and make a new file.\n\n-rm\t\tIf specified in the args, will delete input files which are deemed empty (because of the '-db' arg)");
        exit(0);
    }

    if let Some(db_str) = std::env::args().into_iter().find(|a| a.contains(&db_arg)) {
        db = db_str.strip_prefix(&db_arg).unwrap().parse().unwrap_or(db);
    }

    if let Some(_) = std::env::args().into_iter().find(|a| a == &overwrite_arg) {
        should_overwrite = true;
    }

    if let Some(_) = std::env::args().into_iter().find(|a| a == &delete_empty_arg) {
        delete_empty = true;
    }

    (db, should_overwrite, delete_empty)
}

/// **Returns** the largest number that an `x` bit `(signed?)` integer can store.
/// # Example
/// ```rust
/// assert_eq!(int_bit_to_max(16, true) as i16, i16::MAX);
/// assert_eq!(int_bit_to_max(16, false) as u16, u16::MAX);
/// assert_eq!(int_bit_to_max(24, true) as i32, 8388607_i32); // looking for the max of 24 bit, so must be stored in a bigger type
/// assert_eq!(int_bit_to_max(24, false) as u32, 16777215_u32);
/// int_bit_to_max
/// ```
fn int_bit_to_max(bits: u32, signed: bool) -> u64 {
    let sign = {
        if signed {
            1
        } else {
            0
        }
    };

    2u64.pow(bits as u32 - sign) - 1
}

/// Returns the normalized value from decibels.
/// # Example
/// ```rust
/// assert_eq!(db_to_normalized_value(0.0), 1.0);
/// assert_eq!(db_to_normalized_value(-2.0), 0.63095734448);
/// assert_eq!(db_to_normalized_value(-60.0), 0.000001);
/// ```
#[allow(non_snake_case)]
fn db_to_normalized_value(dB: f32) -> f32 {
    10_f32.powf(dB/20f32)
}

#[test]
fn bit_to_max () {
    assert_eq!(int_bit_to_max(16, true) as i16, i16::MAX);
    assert_eq!(int_bit_to_max(16, false) as u16, u16::MAX);
    assert_eq!(int_bit_to_max(24, true) as i32, 8388607_i32);
    assert_eq!(int_bit_to_max(24, false) as u32, 16777215_u32);
    assert_eq!(int_bit_to_max(32, true) as i32, i32::MAX);
    assert_eq!(int_bit_to_max(32, false) as u32, u32::MAX);
}

#[test]
fn db_to_normalized() {
    return ();
    assert_eq!(db_to_normalized_value(0.0), 1.0);
    assert_eq!(db_to_normalized_value(-2.0), 0.63095734448);
    assert_eq!(db_to_normalized_value(-60.0), 0.000001);
}
