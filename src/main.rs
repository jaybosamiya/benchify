#![allow(unused_imports, dead_code)]

use clap::Clap;
use color_eyre::eyre::{self, eyre, Result};
use indicatif::{HumanDuration, MultiProgress, ProgressBar, ProgressStyle};
use log::{debug, error, info, trace, warn}; // error >> warn >> info >> debug >> trace
use serde::{Deserialize, Serialize};

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

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
}

type Args = Vec<String>;

type ShellCommand = String;

#[derive(Deserialize, Serialize, Debug)]
pub struct Runner {
    warmup: Option<u32>,
    prepare: Option<ShellCommand>,
    run: Args,
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
        if self.run.iter().any(|a| a.contains("{FILE}")) {
            return true;
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
    fn run_cmd(&self, cmdtype: &str, test: &Test, cmd: &ShellCommand) -> Result<()> {
        trace!("{} of tool {} for {}", cmdtype, self.name, &test.tag);
        let cmd = test.interpolated_into(cmd);
        trace!("Running `{}`", cmd);
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .output()?;
        if output.status.success() {
            trace!("{} generated output\n{:?}", cmdtype, output);
        } else {
            error!(
                "{} of {} for {} failed with status code {}",
                cmdtype, self.name, test.tag, output.status
            )
        }
        Ok(())
    }

    pub fn prepare(&self, test: &Test, global_warmup: Option<u32>) -> Result<()> {
        let runner = &self.runners[&test.tag];
        if let Some(cmd) = &runner.prepare {
            self.run_cmd("Preparation", test, cmd)?;
            if let Some(warmup) = runner.warmup.or(global_warmup) {
                info!("Performing {} warmup runs", warmup);
                for _ in 0..warmup {
                    self.run(test)?;
                }
            }
            Ok(())
        } else {
            Ok(())
        }
    }

    pub fn run(&self, test: &Test) -> Result<std::time::Duration> {
        let args = test.interpolated_into_args(&self.runners[&test.tag].run);
        trace!("Running {} with args {:?}", self.program, args);
        let timer = std::time::Instant::now();
        let output = std::process::Command::new(&self.program)
            .args(args)
            .output()?;
        let elapsed_time = timer.elapsed();
        if output.status.success() {
            trace!("Generated output\n{:?}", output);
            info!("Ran {} in {} ms", self.name, elapsed_time.as_millis());
        } else {
            error!("Command exited with non zero status code {}", output.status)
        }
        Ok(elapsed_time)
    }

    pub fn cleanup(&self, test: &Test) -> Result<()> {
        if let Some(cmd) = &self.runners[&test.tag].cleanup {
            self.run_cmd("Clean up", test, cmd)
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
}

impl Test {
    pub fn interpolated_into(&self, s: &str) -> String {
        let extra_args = &self.extra_args.as_ref().unwrap_or(&vec![]).join(" ");
        let s = s
            .replace("{NAME}", &self.name)
            .replace("{TAG}", &self.tag)
            .replace("\"{...}\"", &extra_args)
            .replace("'{...}'", &extra_args);
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
    warmup: Option<u32>,
    min_runs: Option<u32>,
    max_runs: Option<u32>,
    benchify_version: usize,
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

        let mut tag_needs_file_due_to = HashMap::new();

        for tool in &self.tools {
            debug!("Confirming sanity for tool {}", tool.name);

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

    fn get_timings(&self, test: &Test, tool: &Tool) -> Result<Vec<std::time::Duration>> {
        let num_initial_estimates = self.max_runs().min(2) as usize;

        let expected_time_seconds = 2.5f32;

        let pb_style = ProgressStyle::default_bar()
            .template(
                "{spinner:.green} {msg} \
                     [{bar:40.cyan/blue}] {pos}/{len} ({elapsed} -- ETA {eta})",
            )
            .progress_chars("#>-");

        let pb = ProgressBar::new(num_initial_estimates as u64);
        pb.set_style(pb_style.clone());
        pb.set_message(&format!(
            "[{}] [{}]\tInitial estimates",
            test.name, tool.name
        ));
        let initial_estimates = (0..num_initial_estimates)
            .map(|_| {
                pb.inc(1);
                tool.run(test)
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
        pb.set_message(&format!("[{}] [{}]\tBenchmarking", test.name, tool.name));
        pb.set_style(pb_style);
        let remaining_iterations = (num_initial_estimates..preferred_number_of_iterations as usize)
            .map(|i| {
                pb.set_position(i as u64);
                tool.run(test)
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

        Ok(BenchifyResults {
            results: self
                .tests
                .iter()
                .map(|test| {
                    info!("Running tests for {}", test.name);
                    debug!("Test: {:?}", test);

                    Ok((
                        test.name.clone(),
                        self.tools
                            .iter()
                            .map(|tool| {
                                info!("Testing tool {}", tool.name);
                                trace!("Tool: {:?}", tool.runners[&test.tag]);

                                tool.prepare(test, self.warmup)?;
                                let timings = self.get_timings(test, tool)?;
                                tool.cleanup(test)?;

                                Ok((tool.name.clone(), timings))
                            })
                            .collect::<Result<_>>()?,
                    ))
                })
                .collect::<Result<_>>()?,
        })
    }
}

#[derive(Debug)]
pub struct BenchifyResults {
    // test -> (executor -> [timing])
    results: HashMap<String, HashMap<String, Vec<std::time::Duration>>>,
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

    let config: BenchifyConfig = toml::from_str(
        &std::fs::read_to_string(&opts.benchify_toml)
            .or(Err(eyre!("Could not read {:?}", &opts.benchify_toml)))?,
    )?;

    let results = config.execute()?;
    println!("{:?}", results);

    Ok(())
}
