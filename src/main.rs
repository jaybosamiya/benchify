#![allow(unused_imports, dead_code)]

use clap::Clap;
use color_eyre::eyre::{self, eyre, Result};
use indicatif::{HumanDuration, MultiProgress, ProgressBar, ProgressStyle};
use log::{debug, error, info, trace, warn}; // error >> warn >> info >> debug >> trace
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

const PROGRAM_NAME: &'static str = env!("CARGO_PKG_NAME", "expected to be built with cargo");
const PROGRAM_VERSION: &'static str = env!("CARGO_PKG_VERSION", "expected to be built with cargo");
const PROGRAM_AUTHORS: &'static str = env!("CARGO_PKG_AUTHORS", "expected to be built with cargo");

/// A convenient benchmarking tool
#[derive(Clap, Debug)]
#[clap(version = PROGRAM_VERSION, author = PROGRAM_AUTHORS)]
struct CmdLineOpts {
    /// Path to benchify.toml file
    #[clap(default_value = "./benchify.toml")]
    benchify_toml: PathBuf,
    /// Generate template benchify.toml file
    #[clap(long)]
    template: bool,
}

type Args = Vec<String>;

type ShellCommand = String;

#[derive(Deserialize, Serialize, Debug)]
pub struct Runner {
    warmup: Option<u32>,
    prepare: Option<ShellCommand>,
    run_args: Option<Args>,
    run_cmd: Option<ShellCommand>,
    cleanup: Option<ShellCommand>,
}

impl Runner {
    pub fn needs_file(&self) -> bool {
        if let Some(cmd) = &self.prepare {
            if cmd.contains("{FILE}") {
                return true;
            }
        }
        if let Some(cmd) = &self.cleanup {
            if cmd.contains("{FILE}") {
                return true;
            }
        }
        if let Some(run_args) = &self.run_args {
            if run_args.iter().any(|a| a.contains("{FILE}")) {
                return true;
            }
        }
        if let Some(run_cmd) = &self.run_cmd {
            if run_cmd.contains("{FILE}") {
                return true;
            }
        }
        false
    }
}

pub type Tag = String;

#[derive(Deserialize, Serialize, Debug)]
pub struct Tool {
    name: String,
    program: String,
    existence_confirmation: Option<Args>,
    install_instructions: String,
    runners: HashMap<Tag, Runner>,
}

impl Tool {
    fn run_cmd(
        &self,
        cmdtype: &str,
        test: &Test,
        cmd: &ShellCommand,
        opb: Option<ProgressBar>,
    ) -> Result<()> {
        let pb = if let Some(opb) = opb {
            opb
        } else {
            ProgressBar::new_spinner()
        };
        pb.set_style(
            ProgressStyle::default_spinner().template("{spinner:.green} {msg} ({elapsed_precise})"),
        );
        pb.set_message(&format!("[{}] [{}] {}", test.name, self.name, cmdtype));

        trace!("{} of tool {} for {}", cmdtype, self.name, &test.tag);
        let cmd = test.interpolated_into(cmd);
        trace!("Running `{}`", cmd);
        let mut process = std::process::Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        let status = loop {
            match process.try_wait()? {
                None => {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    pb.tick();
                }
                Some(status) => {
                    break status;
                }
            }
        };
        if status.success() {
            trace!("{} exited successfully", cmdtype);
        } else {
            error!(
                "{} of {} for {} failed with status code {}",
                cmdtype, self.name, test.tag, status
            );
            return Err(eyre!(
                "{} of {} for {} failed with status code {}",
                cmdtype,
                self.name,
                test.tag,
                status
            ));
        }
        pb.finish_and_clear();

        Ok(())
    }

    pub fn prepare(&self, test: &Test, opb: Option<ProgressBar>) -> Result<()> {
        let runner = &self.runners[&test.tag];
        if let Some(cmd) = &runner.prepare {
            self.run_cmd("Preparation", test, cmd, opb)?;
            Ok(())
        } else {
            Ok(())
        }
    }

    pub fn run(&self, test: &Test) -> Result<std::time::Duration> {
        let stdin = if let Some(cmd) = &test.stdin_from_cmd {
            use std::os::unix::io::{AsRawFd, FromRawFd};
            let cmd = std::process::Command::new("sh")
                .arg("-c")
                .arg(test.interpolated_into(cmd))
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()?;
            cmd.stdout.unwrap().into()
        } else {
            std::process::Stdio::null()
        };
        let runner = &self.runners[&test.tag];
        let (timer, output) = if let Some(run_args) = &runner.run_args {
            let args = test.interpolated_into_args(run_args);
            trace!("Running {} with args {:?}", self.program, args);
            let timer = std::time::Instant::now();
            let output = std::process::Command::new(&self.program)
                .args(args)
                .stdin(stdin)
                .output()?;
            (timer, output)
        } else if let Some(run_cmd) = &runner.run_cmd {
            let cmd = test.interpolated_into(run_cmd);
            trace!("Running {} with shell command {:?}", self.program, cmd);
            let timer = std::time::Instant::now();
            let output = std::process::Command::new("sh")
                .arg("-c")
                .arg(cmd)
                .stdin(stdin)
                .output()?;
            (timer, output)
        } else {
            unreachable!()
        };
        let elapsed_time = timer.elapsed();
        if output.status.success() {
            trace!("Generated output\n{:?}", output);
            info!("Ran {} in {} ms", self.name, elapsed_time.as_millis());
        } else {
            error!("Command exited with non zero status code {}", output.status);
            return Err(eyre!(
                "Command exited with non zero status code {}",
                output.status
            ));
        }
        if let Some(true) = test.stdout_is_timing {
            let timing = std::time::Duration::from_secs_f64(
                String::from_utf8(output.stdout.to_owned())?
                    .trim()
                    .parse()?,
            );
            if timing > elapsed_time {
                Err(eyre!(
                    "Program lied about elapsed time at stdout: {:?} is not less than {:?}",
                    timing,
                    elapsed_time
                ))
            } else {
                Ok(timing)
            }
        } else {
            Ok(elapsed_time)
        }
    }

    pub fn cleanup(&self, test: &Test) -> Result<()> {
        if let Some(cmd) = &self.runners[&test.tag].cleanup {
            self.run_cmd("Clean up", test, cmd, None)
        } else {
            Ok(())
        }
    }
}

#[derive(Deserialize, Serialize, Debug)]
pub struct Test {
    name: String,
    tag: Tag,
    file: Option<String>,
    extra_args: Option<Vec<String>>,
    stdin_from_cmd: Option<String>,
    stdout_is_timing: Option<bool>,
}

impl Test {
    pub fn interpolated_into(&self, s: &str) -> String {
        let extra_args = &self.extra_args.as_ref().unwrap_or(&vec![]).join(" ");
        let extra_args_quoted = &self
            .extra_args
            .as_ref()
            .unwrap_or(&vec![])
            .iter()
            .map(|x| format!("'{}'", x))
            .collect::<Vec<_>>()
            .join(" ");
        let s = s
            .replace("{NAME}", &self.name)
            .replace("{TAG}", &self.tag)
            .replace("\"{...}\"", &extra_args)
            .replace("'{...}'", &extra_args)
            .replace("{...}", &extra_args_quoted);
        if let Some(file) = &self.file {
            s.replace("{FILE}", file)
        } else {
            s
        }
    }

    pub fn interpolated_into_args(&self, args: &Args) -> Args {
        let mut res = vec![];
        for arg in args {
            if arg == "{...}" || arg == "..." {
                res.append(&mut self.extra_args.as_ref().unwrap_or(&vec![]).clone());
            } else {
                res.push(self.interpolated_into(arg));
            }
        }
        res
    }
}

#[derive(Deserialize, Serialize, Debug)]
pub struct BenchifyConfig {
    benchify_version: usize,
    parallel_prep: Option<bool>,
    warmup: Option<u32>,
    min_runs: Option<u32>,
    max_runs: Option<u32>,
    main_tool: Option<String>,
    results_dir: Option<PathBuf>,
    tags: HashSet<Tag>,
    tools: Vec<Tool>,
    tests: Vec<Test>,
}

impl BenchifyConfig {
    fn min_runs(&self) -> u32 {
        self.min_runs.unwrap_or(10)
    }

    fn max_runs(&self) -> u32 {
        self.max_runs.unwrap_or(1000)
    }

    fn results_dir(&self) -> PathBuf {
        self.results_dir
            .clone()
            .unwrap_or(PathBuf::from("./benchify-results/"))
    }

    fn parallel_prep(&self) -> bool {
        match self.parallel_prep {
            None => false,
            Some(b) => b,
        }
    }

    fn confirm_config_sanity(&self) {
        let mut errored = false;
        if self.benchify_version != 1 {
            errored = true;
            error!(
                "Found config for version {}. Currently only version 1 is supported.",
                self.benchify_version
            )
        }

        if self.min_runs() > self.max_runs() {
            errored = true;
            error!(
                "Min runs ({}) is greater than max runs ({}).",
                self.min_runs(),
                self.max_runs(),
            )
        }

        if self.results_dir().is_file() {
            errored = true;
            error!(
                "Results dir {:?} is already exists as a file.",
                self.results_dir()
            )
        }

        if let Some(tool) = &self.main_tool {
            if !self.tools.iter().any(|t| &t.name == tool) {
                errored = true;
                error!(
                    "Main tool {:?} is not on of the known tools. Expected one of {:?}",
                    tool,
                    self.tools.iter().map(|t| &t.name).collect::<Vec<_>>()
                )
            }
        }

        let mut tag_needs_file_due_to = HashMap::new();

        for tool in &self.tools {
            debug!("Confirming sanity for tool {}", tool.name);

            trace!("Confirmer runner commands");
            for (tag, runner) in &tool.runners {
                if !(runner.run_cmd.is_some() ^ runner.run_args.is_some()) {
                    errored = true;
                    error!(
                        "Runner {:?} for {:?} should have only one of run_cmd and run_args set. \
                         Got {:?} and {:?} respectively.",
                        tag, tool.name, runner.run_cmd, runner.run_args
                    );
                }
            }

            trace!("Confirming tags");
            let tool_tags = tool.runners.keys().cloned().collect();
            if !self.tags.is_subset(&tool_tags) {
                errored = true;
                error!(
                    "Not all runners for {} have been defined. Missing: {:?}",
                    tool.name,
                    tool_tags.difference(&self.tags)
                );
            }
            if !self.tags.is_superset(&tool_tags) {
                errored = true;
                error!(
                    "Invalid set of runner tags found for {}. Found extra: {:?}",
                    tool.name,
                    self.tags.difference(&tool_tags)
                );
            }

            trace!("Confirming runnability");
            let mut ec_cmd = std::process::Command::new(&tool.program);
            let ec_cmd = if let Some(ec_args) = &tool.existence_confirmation {
                ec_cmd.args(ec_args)
            } else {
                &mut ec_cmd
            };

            if ec_cmd.output().is_err() {
                info!(
                    "Ran {} with args {:?}",
                    tool.program, tool.existence_confirmation
                );
                errored = true;
                error!(
                    "Could not confirm that {} can be executed.\n\t\
                     Suggested install instructions:\n\t\t\t{}\n",
                    tool.name, tool.install_instructions,
                );
            }

            trace!("Collecting tags that require files");
            for tag in &self.tags {
                let runner = &tool.runners[tag];
                if runner.needs_file() {
                    tag_needs_file_due_to
                        .entry(tag)
                        .or_insert(vec![])
                        .push(&tool.name);
                }
            }
        }

        for test in &self.tests {
            debug!("Confirming sanity for test {}", test.name);

            trace!("Confirming tags");
            if !self.tags.contains(&test.tag) {
                errored = true;
                error!(
                    "Invalid tag {} for test {}. Expected one of {:?}",
                    test.tag, test.name, self.tags
                );
            }

            if let Some(file) = &test.file {
                trace!("Confirming file existence");
                if !std::path::Path::new(file).exists() {
                    errored = true;
                    error!(
                        "Could not find file {} for test {}. Are you sure it exists?",
                        file, test.name
                    );
                }
            } else if tag_needs_file_due_to.contains_key(&test.tag) {
                errored = true;
                error!(
                    "Test {} needs a file specified due to runner(s): {:?}",
                    test.name, tag_needs_file_due_to[&test.tag]
                );
            }
        }

        if errored {
            std::process::exit(1);
        }
    }

    fn get_timings(
        &self,
        test: &Test,
        tool: &Tool,
        global_warmup: Option<u32>,
    ) -> Result<Vec<std::time::Duration>> {
        let num_initial_estimates = self.max_runs().min(2) as usize;

        let expected_time_seconds = 2.5f32;

        let pb_style = ProgressStyle::default_bar()
            .template(
                "{spinner:.green} {msg} \
                     [{wide_bar:.cyan/blue}] {pos}/{len} ({elapsed} -- ETA {eta})",
            )
            .progress_chars("#>-");

        if let Some(warmup_runs) = tool.runners[&test.tag].warmup.or(global_warmup) {
            let pb = ProgressBar::new(warmup_runs as u64);
            pb.set_style(pb_style.clone());
            pb.set_message(&format!("[{}] [{}] Warmup runs", test.name, tool.name));
            for _ in 0..warmup_runs {
                pb.inc(1);
                tool.run(test).map_err(|e| {
                    pb.set_style(ProgressStyle::default_bar().template("{spinner:.green} {msg}"));
                    pb.finish_with_message(&format!(
                        "[{}] [{}] Failure during warmup: {}",
                        test.name, tool.name, e
                    ));
                    e
                })?;
            }
            pb.finish_and_clear();
        }

        let pb = ProgressBar::new(num_initial_estimates as u64);
        pb.set_style(pb_style.clone());
        pb.set_message(&format!(
            "[{}] [{}] Initial estimates",
            test.name, tool.name
        ));
        let initial_estimates = (0..num_initial_estimates)
            .map(|_| {
                pb.inc(1);
                tool.run(test).map_err(|e| {
                    pb.set_style(ProgressStyle::default_bar().template("{spinner:.green} {msg}"));
                    pb.finish_with_message(&format!(
                        "[{}] [{}] Failure during initial estimates: {}",
                        test.name, tool.name, e
                    ));
                    e
                })
            })
            .collect::<Result<Vec<_>>>()?;
        pb.finish_and_clear();

        let mean_estimated_time_per_iter_secs = initial_estimates
            .iter()
            .map(|t| t.as_secs_f32())
            .sum::<f32>()
            / num_initial_estimates as f32;

        let preferred_number_of_iterations = self.max_runs().min(
            self.min_runs()
                .max((expected_time_seconds / mean_estimated_time_per_iter_secs) as _),
        );

        let pb = ProgressBar::new(preferred_number_of_iterations as u64);
        pb.set_message(&format!("[{}] [{}] Benchmarking", test.name, tool.name));
        pb.set_style(pb_style);
        let remaining_iterations = (num_initial_estimates..preferred_number_of_iterations as usize)
            .map(|i| {
                pb.set_position(i as u64);
                tool.run(test).map_err(|e| {
                    pb.set_style(ProgressStyle::default_bar().template("{spinner:.green} {msg}"));
                    pb.finish_with_message(&format!(
                        "[{}] [{}] Failure during benchmarking run#{}: {}",
                        test.name, tool.name, i, e
                    ));
                    e
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let timings: Vec<_> = initial_estimates
            .into_iter()
            .chain(remaining_iterations.into_iter())
            .collect();
        let mean_timing = timings.iter().sum::<std::time::Duration>() / (timings.len() as u32);
        pb.set_style(ProgressStyle::default_bar().template("{spinner:.green} {msg}"));
        pb.finish_with_message(&format!(
            "[{}] [{}]\tMean {:?} in {} runs",
            test.name,
            tool.name,
            mean_timing,
            timings.len()
        ));

        Ok(timings)
    }

    pub fn execute(&self) -> Result<BenchifyResults> {
        self.confirm_config_sanity();

        if self.parallel_prep() {
            // Run all preparation in parallel
            let mpb = MultiProgress::new();
            let mut t_t_pb = self
                .tests
                .iter()
                .map(|test| {
                    self.tools
                        .iter()
                        .map(|tool| (test, tool, Some(mpb.add(ProgressBar::new_spinner()))))
                        .collect::<Vec<(_, _, _)>>()
                })
                .flatten()
                .collect::<Vec<(_, _, _)>>();
            let mpb_thread = std::thread::spawn(move || mpb.join_and_clear());
            if !t_t_pb
                .par_iter_mut()
                .all(|(test, tool, pb)| tool.prepare(test, pb.take()).is_ok())
            {
                error!("Preparation failed");
                std::process::exit(1);
            }
            mpb_thread.join().unwrap()?;
        }

        Ok(BenchifyResults {
            results: self
                .tests
                .iter()
                .map(|test| {
                    info!("Running tests for {}", test.name);
                    debug!("Test: {:?}", test);

                    self.tools.iter().map(move |tool| {
                        info!("Testing tool {}", tool.name);
                        trace!("Tool: {:?}", tool.runners[&test.tag]);

                        if !self.parallel_prep() {
                            tool.prepare(test, None)?;
                        }
                        let timings = self.get_timings(test, tool, self.warmup);
                        tool.cleanup(test)?;

                        Ok((test.name.as_ref(), tool.name.as_ref(), timings))
                    })
                })
                .flatten()
                .collect::<Result<Vec<_>>>()?,
            main_tool: self.main_tool.as_ref().map(String::as_ref),
        })
    }
}

#[derive(Debug)]
pub struct BenchifyResults<'a> {
    // (test, executor, [timing])
    results: Vec<(&'a str, &'a str, Result<Vec<std::time::Duration>>)>,
    main_tool: Option<&'a str>,
}

fn format_summary(
    main: Option<&str>,
    results: Vec<(&str, Result<&[std::time::Duration]>)>,
) -> Result<String> {
    use std::fmt::Write;

    let mut result = String::new();
    let summaries = results
        .iter()
        .map(|(n, t)| (n, t.as_ref().map(|t| Statistics::new(t))));
    let comparison_point = if let Some(main) = main {
        summaries
            .clone()
            .find(|(t, _s)| *t == &main)
            .map(|(t, s)| (t, s.unwrap()))
            .unwrap()
    } else {
        summaries
            .clone()
            .filter(|(_t, s)| s.is_ok())
            .map(|(t, s)| (t, s.unwrap()))
            .min_by_key(|(_t, s)| s.mean)
            .unwrap()
    };
    let summaries = summaries.map(|(n, stats)| {
        let name = if comparison_point.0 == n {
            format!("**{}**", n)
        } else {
            n.to_string()
        };
        match stats {
            Ok(stats) => {
                let mean = format!("{:.3}", stats.mean.as_secs_f64() * 1000.);
                let stddev = format!("{:.3}", stats.sample_stddev.as_secs_f64() * 1000.);
                let ratio = format!(
                    "{:.3}",
                    stats.mean.as_secs_f64() / comparison_point.1.mean.as_secs_f64()
                );
                (name, mean, stddev, ratio)
            }
            Err(e) => (name, "FAIL".to_string(), "FAIL".to_string(), e.to_string()),
        }
    });
    let lengths = summaries
        .clone()
        .chain(std::iter::once((
            "".to_string(),
            "Mean (ms)".to_string(),
            "StdDev (ms)".to_string(),
            "Ratio".to_string(),
        )))
        .map(|(t, m, s, r)| (t.len(), m.len(), s.len(), r.len()));
    let name_length = lengths.clone().map(|l| l.0).max().unwrap();
    let mean_length = lengths.clone().map(|l| l.1).max().unwrap();
    let stddev_length = lengths.clone().map(|l| l.2).max().unwrap();
    let ratio_length = lengths.clone().map(|l| l.3).max().unwrap();

    writeln!(
        &mut result,
        "| {n: <nl$} | {m: <ml$} ± {s: <sl$} | {r: <rl$} |",
        nl = name_length,
        n = "",
        ml = mean_length,
        m = "Mean (ms)",
        sl = stddev_length,
        s = "StdDev (ms)",
        rl = ratio_length,
        r = "Ratio",
    )?;
    writeln!(
        &mut result,
        "|:{dash:-<nl$}-|-{dash:-<ml$}---{dash:-<sl$}:|-{dash:-<rl$}:|",
        dash = "-",
        nl = name_length,
        ml = mean_length,
        sl = stddev_length,
        rl = ratio_length,
    )?;
    for (name, mean, stddev, ratio) in summaries {
        writeln!(
            &mut result,
            "| {n: <nl$} | {m: >ml$} ± {s: >sl$} | {r: >rl$} |",
            nl = name_length,
            n = name,
            ml = mean_length,
            m = mean,
            sl = stddev_length,
            s = stddev,
            rl = ratio_length,
            r = ratio,
        )?;
    }
    Ok(result)
}

impl<'a> BenchifyResults<'a> {
    fn save_to_directory(&self, results_dir: &Path) -> Result<()> {
        // Make sure the results directory exists
        std::fs::create_dir_all(results_dir)?;
        assert!(results_dir.is_dir());

        {
            // Write out all the data
            let mut data_writer = csv::Writer::from_path(results_dir.join("data.csv"))?;
            data_writer.write_record(&["Test", "Executor", "Timing (s)"])?;
            for (test, executor, timings) in self.results.iter() {
                if let Ok(timings) = timings {
                    for timing in timings.iter() {
                        data_writer.serialize((test, executor, timing.as_secs_f64()))?;
                    }
                }
            }
            data_writer.flush()?;
        }

        for (test, results) in self.results_by_test() {
            // Write out data for each test
            use std::io::Write;
            let mut file = std::fs::File::create(results_dir.join(format!("summary_{}.md", test)))?;
            writeln!(file, "# Summary of runs for {}", test)?;
            writeln!(file)?;
            write!(file, "{}", format_summary(self.main_tool, results)?)?;
        }

        Ok(())
    }

    fn results_by_test(&self) -> Vec<(&'a str, Vec<(&'a str, Result<&[std::time::Duration]>)>)> {
        let mut mapped = HashMap::new();
        for (test, executor, timings) in self.results.iter() {
            mapped.entry(*test).or_insert(vec![]).push((*executor, {
                timings.as_deref().map_err(|e| eyre!("{}", e))
            }));
        }
        let mut res = vec![];
        for (test, _executor, _timings) in self.results.iter() {
            if let Some((k, v)) = mapped.remove_entry(test) {
                res.push((k, v));
            }
        }
        res
    }

    fn results_by_executor(&self) -> Vec<(&'a str, Vec<(&'a str, &[std::time::Duration])>)> {
        let mut mapped = HashMap::new();
        for (test, executor, timings) in self.results.iter() {
            if let Ok(timings) = timings {
                mapped
                    .entry(*executor)
                    .or_insert(vec![])
                    .push((*test, timings.as_ref()));
            }
        }
        let mut res = vec![];
        for (_test, executor, _timings) in self.results.iter() {
            if let Some((k, v)) = mapped.remove_entry(executor) {
                res.push((k, v));
            }
        }
        res
    }

    fn display_summary(&self) -> Result<()> {
        for (test, results) in self.results_by_test() {
            println!();
            println!("# {}", test);
            println!();
            print!("{}", format_summary(self.main_tool, results)?);
            println!();
        }

        Ok(())
    }
}

#[derive(Debug)]
struct Statistics {
    mean: std::time::Duration,
    sample_stddev: std::time::Duration,
    min: std::time::Duration,
    max: std::time::Duration,
    count: usize,
}

impl Statistics {
    fn new(data: &[std::time::Duration]) -> Self {
        use std::cmp::{max, min};
        use std::iter::Sum;

        let count = data.len();
        assert_ne!(count, 0);
        let mean = std::time::Duration::sum(data.iter()) / (count as u32);
        let sample_variance = (data
            .iter()
            .map(|t| (t.as_secs_f64() - mean.as_secs_f64()).powf(2.))
            .sum::<f64>())
            / ((data.len() - 1) as f64).powf(2.);
        let sample_stddev = std::time::Duration::from_secs_f64(sample_variance.sqrt());
        let min = *data.iter().min().unwrap();
        let max = *data.iter().max().unwrap();

        Statistics {
            mean,
            sample_stddev,
            min,
            max,
            count,
        }
    }
}

fn main() -> Result<()> {
    color_eyre::install()?;
    pretty_env_logger::init();

    {
        let template_toml = include_str!("template.toml");
        if let Ok(_template_config) = toml::from_str::<BenchifyConfig>(template_toml) {
        } else {
            panic!("Benchify seems horribly broken somehow. Try getting a recent version.")
        }
    }

    let opts = CmdLineOpts::parse();

    if opts.template {
        if opts.benchify_toml.exists() {
            error!("{:?} already exists. Not overwriting.", opts.benchify_toml);
            std::process::exit(1);
        } else {
            std::fs::write(opts.benchify_toml, include_str!("template.toml"))?;
        }
    } else {
        let config: BenchifyConfig = toml::from_str(
            &std::fs::read_to_string(&opts.benchify_toml)
                .or(Err(eyre!("Could not read {:?}", &opts.benchify_toml)))?,
        )?;

        let results = config.execute()?;
        results.save_to_directory(&config.results_dir())?;
        results.display_summary()?;
    }

    Ok(())
}
