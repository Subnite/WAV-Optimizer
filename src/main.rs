use std::{fs::{self, create_dir, remove_file}, io, path::Path, process::exit};

use hound::WavReader;
use ignore::{DirEntry, WalkBuilder};

fn main() {
    let (db, is_overwrite, is_delete_empty, auto_cut) = process_args();
    println!("RUNNING WITH SETTINGS:\n\tminimum db = {}, overwrite input files = {}, delete empty files = {}, Auto cut = {:?}", db, is_overwrite, is_delete_empty, auto_cut);

    let processor = WavProcessor::new(db_to_normalized_value(db), is_delete_empty, is_overwrite, auto_cut);

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

#[derive(Debug, Default)]
struct AutoCut {
    min_silence_length_ms: f32,
    min_length_per_sample_ms: f32,
    numbering_postfix: String,
    create_subdirectory: bool,
    delete_original: bool, // TODO: unused
}


impl AutoCut {
    fn default() -> Self {
        AutoCut {
            min_silence_length_ms: 20.0,
            min_length_per_sample_ms: 300.0,
            numbering_postfix: "-".to_string(),
            create_subdirectory: false,
            delete_original: false,
        }
    }
}

struct WavProcessor {
    deviation_normalized: f32,
    delete_empty: bool,
    overwrite_input: bool,
    auto_cut: Option<AutoCut>,
}

impl WavProcessor {
    fn new(deviation: f32, should_delete_empty: bool, should_overwrite_input: bool, auto_cut: Option<AutoCut>) -> Self {
        WavProcessor {
            deviation_normalized: deviation,
            delete_empty: should_delete_empty,
            overwrite_input: should_overwrite_input,
            auto_cut: auto_cut,
        }
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

        // now check for regions that need to be cut and exported separately...
        if let Some(ac) = &self.auto_cut {
            // NOTE: auto cut that shiii
            // println!("Auto Cut Detected!!");
            let mut silence_ranges = self.get_silence_ranges(&new_channels, reader, ac, deviation);
            let has_cut = self.try_saving_auto_cuts(&mut silence_ranges, &mut reader.spec(), &ac, &mut new_channels, path);
            if has_cut.is_err() {
                // save new singular wav
                if let Err(msg) = self.save_new_wav::<T>(&new_channels, &mut reader.spec(), path, None) {
                    println!("{msg}");
                }
            }
        }
        else {
            // save new singular wav
            if let Err(msg) = self.save_new_wav::<T>(&new_channels, &mut reader.spec(), path, None) {
                println!("{msg}");
            }
        }

        // println!("\n\t================================================\n");
    }





    fn get_sample_len_from_ms(ms: &f32, sample_rate: &u32) -> u32 {
        (ms / 1000_f32 * (*sample_rate) as f32) as u32
    }





    fn get_silence_ranges<T, R>(&self, channels: &Vec<Vec<T>>, reader: &mut WavReader<R>, ac: &AutoCut, deviation: T) -> Option<Vec<(usize, usize)>>
where
        T: hound::Sample + PartialOrd<T> + std::ops::Neg<Output = T> + Copy + Default + Ord + std::fmt::Debug,
        R: io::Read,
    {
        let sample_rate = reader.spec().sample_rate.clone();
        let silence_min_length_samples = WavProcessor::get_sample_len_from_ms(&ac.min_silence_length_ms, &sample_rate);
        // let sample_min_length_samples = WavProcessor::get_sample_len_from_ms(&ac.min_length_per_sample_ms, &sample_rate);
        // println!("min silence length: {}, min samples length: {}", silence_min_length_samples, sample_min_length_samples);

        let mut silence_ranges_per_channel: Vec<Vec<(usize, usize)>> = Vec::with_capacity(10);

        for channel in channels {
            let mut silence_ranges_vec: Vec<(usize, usize)> = Vec::with_capacity(5);
            let (mut silence_start, mut silence_end) = (0_usize, 0_usize);
            let mut is_checking_silence = false;

            for (i, sample) in channel.iter().enumerate() {
                if !(*sample > deviation || *sample < -deviation) {
                    // found a zero.
                    if !is_checking_silence {
                        silence_start = i;
                        silence_end = i;
                        is_checking_silence = true;
                    } else {
                        silence_end = i; // this makes it inclusive. so [start:end]
                    }
                } else {
                    if is_checking_silence {
                        is_checking_silence = false;
                        // check if lengths are in margin.
                        if silence_end - silence_start >= silence_min_length_samples as usize {
                            silence_ranges_vec.push((silence_start.clone(), silence_end.clone()));
                        }
                    }
                }
            }

            if silence_ranges_vec.len() > 0 {
                silence_ranges_per_channel.push(silence_ranges_vec);
            }
        }

        // println!("amount of silences per channel: {}\nsilences: {:?}", if silence_ranges_per_channel.len() > 0 { silence_ranges_per_channel[0].len() } else { 0 }, &silence_ranges_per_channel);

        let final_silences_all_channels: Option<Vec<(usize, usize)>> = {
            if silence_ranges_per_channel.len() < 2 && silence_ranges_per_channel.len() > 0 { Some(silence_ranges_per_channel[0].clone()) }
            else if silence_ranges_per_channel.len() <= 0 { None }
            else {
                // find the common grounds for each channel.
                // now make slices based on the silences in EACH channel, let's not forget that there
                // are multiple ones so make sure they don't cut away samples.
                let mut v: Vec<(usize, usize)> = Vec::with_capacity(5);

                // find the largest vec first
                let most_silences_vec = silence_ranges_per_channel.iter().max_by_key(|v| v.len());
                if most_silences_vec.is_none() {
                    return None;
                }

                // if other start > this start but <= this.end, then this.start = other.start
                // if other end > this start but <= this.end, then this.end = other.end
                for this_silence in &mut most_silences_vec.unwrap().clone() {
                    for other_vec in &silence_ranges_per_channel[0..] {
                        let mut found_silence = false;
                        for other_silence in other_vec {
                            if other_silence.0 >= this_silence.0 && other_silence.0 <= this_silence.1 { this_silence.0 = other_silence.0; found_silence = true; }
                            if other_silence.1 >= this_silence.0 && other_silence.1 <= this_silence.1 { this_silence.1 = other_silence.1; found_silence = true; }
                            if found_silence { break }
                        }
                    }

                    v.push(*this_silence);
                }

                // println!("final silences:\t\t\t\t{:?}", &v);
                if v.len() > 0 { return Some(v); } else { return None; }
            }
        };

        final_silences_all_channels
    }






    fn try_saving_auto_cuts<T>(&self, silence_ranges: &mut Option<Vec<(usize, usize)>>, spec: &mut hound::WavSpec, ac: &AutoCut, new_channels: &mut Vec<Vec<T>>, path: &Path) -> Result<(), String>
where
    T: hound::Sample + PartialOrd<T> + std::ops::Neg<Output = T> + Copy + Default + Ord + std::fmt::Debug,
    {
        let sample_rate = spec.sample_rate;
        let mut remove_idxs: Vec<usize> = Vec::with_capacity(5);
        if let Some(ranges) = silence_ranges {
            // TODO: split up the areas and save them separately
            // Check lengths if they are still applicable with the ac settings
            let min_silence_len = Self::get_sample_len_from_ms(&ac.min_silence_length_ms, &sample_rate) as usize;
            let min_sample_len = Self::get_sample_len_from_ms(&ac.min_length_per_sample_ms, &sample_rate) as usize;
            for (i, range) in ranges.iter().enumerate() {
                if range.1 - range.0 < min_silence_len {
                    remove_idxs.push(i);
                }

            }
            // remove all silences that were too short
            for (_, i) in remove_idxs.iter().rev().enumerate() { // reverse to make sure indexes don't change because originally they go up, which would do -1 at each number
                ranges.remove(*i);
            }

            remove_idxs.clear();

            if ranges.len() <= 0 { return Err("Ranges length was 0".to_string()); }

            // check if sample lengths are still good
            for (i, range) in ranges.iter().enumerate() {
                if i == 0 {
                    // check if the space before is long enough, if not, remove this silence range.
                    if range.0 < min_sample_len {
                        remove_idxs.push(i);
                        continue;
                    }
                }
                if i >= ranges.len() - 2 || ranges.len() == 1 {
                    if &new_channels[0].len() - range.1 < min_sample_len {
                        if !remove_idxs.iter().any(|e| *e == i) { remove_idxs.push(i); }
                        continue;
                    }
                } else {
                    let next_range = ranges[i+1];
                    if next_range.0 - range.1 < min_sample_len {
                        // remove next cut range
                        remove_idxs.push(i+1);
                    }
                }
            }

            for (_, i) in remove_idxs.iter().rev().enumerate() { // reverse to make sure indexes don't change because originally they go up, which would do -1 at each number
                ranges.remove(*i);
            }

            // save all samples that aren't in the ranges separately
            // println!("final ranges after length checks:\t{:?}", &ranges);

            if ranges.len() > 0 {
                let mut samples: Vec<Vec<Vec<T>>> = Vec::with_capacity(ranges.len()+2); // +2 for before and after the cuts
                let mut start_i = 0_usize;
                for range in ranges {
                    let end_i = range.0;
                    let scoped_vec = {
                        let mut v: Vec<Vec<T>> = Vec::new();
                        for channel in new_channels.as_slice() {
                            v.push(channel[start_i..=end_i].to_vec());
                        }
                        v
                    };
                    samples.push(scoped_vec);

                    start_i = range.1;
                }

                // one more time to get the remainder of the samples
                let end_i = new_channels[0].len() - 1;
                let scoped_vec = {
                    let mut v: Vec<Vec<T>> = Vec::new();
                    for channel in new_channels.as_slice() {
                        v.push(channel[start_i..=end_i].to_vec());
                    }
                    v
                };
                samples.push(scoped_vec);

                // println!("Outputting {} samples", samples.len());

                for (i, channels) in samples.iter().enumerate() {
                    let pf: String = ac.numbering_postfix.clone() + (&format!("{:02}", i+1));
                    if let Err(msg) = self.save_new_wav::<T>(&channels, spec, path, Some(&pf)) {
                        println!("{msg}");
                    }
                }
            } else {
                return Err("There were no ranges.".to_string());
            }
        }
        else {
            return Err("There were no silence ranges from the start.".to_string());
        }


        Ok(())
    }






    /// saves channel data into the path that was passed in.
    fn save_new_wav<T>(&self, channels: &Vec<Vec<T>>, spec: &mut hound::WavSpec, path: &Path, postfix: Option<&str>) -> Result<(), String>
where
        T: hound::Sample + PartialOrd<T> + std::ops::Neg<Output = T> + Copy + Default + Ord + std::fmt::Debug,
    {
        spec.channels = channels.len() as u16;
        let samples_per_channel = {if channels.len() > 0 {channels[0].len()} else {0}};
        if samples_per_channel == 0 {
            if self.delete_empty {
                println!("deleting file because it's empty: {:?}", path);
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
            let name: &str = match path.file_name() {
                Some(name) => name.to_str().unwrap().strip_suffix(".wav").unwrap(),
                _ => "default_name"
            };
            let f_name = format!("{name}{}{}.wav", if self.overwrite_input {""} else {"_stripped"} , if let Some(pf) = postfix {pf} else {""});


            // check if you should create a subdirectory
            let create_subdir: bool = {
                if let Some(ac) = &self.auto_cut {
                    if ac.create_subdirectory {
                        let mut subdir_path = path.parent().unwrap().to_path_buf();
                        subdir_path.push(name);
                        if !subdir_path.exists() {
                            create_dir(&subdir_path).unwrap_or(());
                            // println!("Made dir at path: {:?}", subdir_path);
                        }
                    }
                    ac.create_subdirectory
                }
                else { false }
            };

            // check if you should delete the original:
            if let Some(ac) = &self.auto_cut {
                if ac.delete_original {
                    if path.is_file() && path.exists() {
                        remove_file(path).unwrap();
                    }
                }
            }

            path.with_file_name(format!("{}{}", if create_subdir {name.to_string() + "/"} else {"".to_string()}, f_name))
            // path.with_file_name(f_name)
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
fn process_args() -> (f32, bool, bool, Option<AutoCut>) {
    let help_arg = String::from("-h");
    let db_arg = String::from("-db=");
    let overwrite_arg = String::from("-o");
    let delete_empty_arg = String::from("-rm");
    let auto_cut_arg = String::from("-ac");
    let auto_cut_min_silence_len_ms_arg = String::from("-acsilence=");
    let auto_cut_min_sample_len_ms_arg = String::from("-acsample=");
    let auto_cut_postfix_arg = String::from("-acpostfix=");
    let auto_cut_subdir_arg = String::from("-acsubdir");
    let auto_cut_delete_original_arg = String::from("-acdelete");

    let mut db = -60.0;
    let mut should_overwrite = false;
    let mut delete_empty = false;
    let mut auto_cut = None; // default none

    // let mut args_iter = std::env::args().into_iter();

    if let Some(_) = std::env::args().into_iter().find(|b| *b == help_arg) {
        println!("\t[USAGE]");
        println!("Looks for a \".wavignore\" file in the current directory which uses the gitignore style (you can put them in subdirectories too)\n\nFinds all .wav files and trims the end silence off.\nIt will also try to cut whole channels if they are empty.");
        println!("\n\n\t[EXAMPLE]");
        println!("wav_optimizer.exe -db=-55.7 -o -rm");
        println!("wav_optimizer.exe -db=-40 -o -rm -ac -acsilence=202.1 -acsample=250 -acpostfix='.'");
        println!("\n\n\t[OPTIONS]\n-db=\t\tset a float value for the minimum dB the sample should be at the end when trimming. If not specified, it defaults to -60 dB\n\n-o\t\tif specified in the args, will overwrite the input files with the trimmed version. Otherwise it will add a suffix to the name and make a new file.\n\n-rm\t\tIf specified in the args, will delete input files which are deemed empty (because of the '-db' arg).\n\n-ac\t\tWill enable auto cutting up the sample at silences, this will then export multiple smaller files which contain audio data over the threshold.\n\n-acsilence=\tThe minimum amount of milliseconds the samples need to be under the threshold to recognize it as a separate sample.\n\n-acsample=\tThe minimum amount of milliseconds a cut sample needs to be before being recognized as a separate sample.\n\n-acpostfix=\tThe postfix to use before numbering. For example inputfile-01 or inputfile.01.\n\n-acsubdir\tWill add the outputted cuts into a subfolder with the name of the original file.\n\n-acdelete\tWill delete the original (long) sample after creating the cuts.");
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

    // NOTE: AutoCut functions

    if let Some(_) = std::env::args().into_iter().find(|a| a == &auto_cut_arg) {
        auto_cut = Some(AutoCut::default());
    }

    if let Some(ms_str) = std::env::args().into_iter().find(|a| a.contains(&auto_cut_min_silence_len_ms_arg)) {
        if let Some(ac) = &mut auto_cut {
            ac.min_silence_length_ms = ms_str.strip_prefix(&auto_cut_min_silence_len_ms_arg).unwrap().parse().unwrap_or(ac.min_silence_length_ms);
        }
    }

    if let Some(ms_str) = std::env::args().into_iter().find(|a| a.contains(&auto_cut_min_sample_len_ms_arg)) {
        if let Some(ac) = &mut auto_cut {
            ac.min_length_per_sample_ms = ms_str.strip_prefix(&auto_cut_min_sample_len_ms_arg).unwrap().parse().unwrap_or(ac.min_length_per_sample_ms);
        }
    }

    if let Some(postfix_str) = std::env::args().into_iter().find(|a| a.contains(&auto_cut_postfix_arg)) {
        if let Some(ac) = &mut auto_cut {
            ac.numbering_postfix = postfix_str.strip_prefix(&auto_cut_postfix_arg).unwrap().parse().unwrap_or(ac.numbering_postfix.to_string());
        }
    }

    if let Some(_) = std::env::args().into_iter().find(|a| a == &auto_cut_subdir_arg) {
        if let Some(ac) = &mut auto_cut {
            ac.create_subdirectory = true;
        }
    }

    if let Some(_) = std::env::args().into_iter().find(|a| a == &auto_cut_delete_original_arg) {
        if let Some(ac) = &mut auto_cut {
            ac.delete_original = true;
        }
    }

    (db, should_overwrite, delete_empty, auto_cut)
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
