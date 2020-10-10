#![allow(unused_imports, dead_code)]

use clap::Clap;
use color_eyre::eyre::{self, eyre, Result};
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

#[derive(Deserialize, Serialize, Debug)]
pub struct Runner {
    prepare: Option<Args>,
    run: Args,
    cleanup: Option<Args>,
}

pub type Tag = String;

#[derive(Deserialize, Serialize, Debug)]
pub struct Tool {
    name: String,
    binary: String,
    existence_confirmation: Args,
    install_instructions: String,
    runners: HashMap<Tag, Runner>,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct Test {
    name: String,
    tag: Tag,
    file: String,
    command: String,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct BenchifyConfig {
    benchify_version: usize,
    tags: HashSet<Tag>,
    tools: Vec<Tool>,
    tests: Vec<Test>,
}

impl BenchifyConfig {
    fn confirm_config_sanity(&self) {
        if self.benchify_version != 1 {
            error!(
                "Found config for version {}. Currently only version 1 is supported.",
                self.benchify_version
            )
        }

        for tool in &self.tools {
            debug!("Confirming sanity for tool {}", tool.name);

            trace!("Confirming tags");
            let tool_tags = tool.runners.keys().cloned().collect();
            if !self.tags.is_subset(&tool_tags) {
                error!(
                    "Not all runners for {} have been defined. Missing: {:?}",
                    tool.name,
                    tool_tags.difference(&self.tags)
                );
            }
            if !self.tags.is_superset(&tool_tags) {
                error!(
                    "Invalid set of runner tags found for {}. Found extra: {:?}",
                    tool.name,
                    self.tags.difference(&tool_tags)
                );
            }

            trace!("Confirming runnability");
            if std::process::Command::new(&tool.binary)
                .args(&tool.existence_confirmation)
                .output()
                .is_err()
            {
                info!(
                    "Ran {} with args {:?}",
                    tool.binary, tool.existence_confirmation
                );
                error!(
                    "Could not confirm that {} is executable.\n\
                            Install instructions: {}",
                    tool.name, tool.install_instructions,
                );
            }
        }

        for test in &self.tests {
            debug!("Confirming sanity for test {}", test.name);

            trace!("Confirming tags");
            if !self.tags.contains(&test.tag) {
                error!(
                    "Invalid tag {} for test {}. Expected one of {:?}",
                    test.tag, test.name, self.tags
                );
            }

            trace!("Confirming file existence");
            if !std::path::Path::new(&test.file).exists() {
                error!(
                    "Could not find file {} for test {}. Are you sure it exists?",
                    test.file, test.name
                );
            }
        }
    }

    pub fn execute(&self) -> Result<BenchifyResults> {
        self.confirm_config_sanity();
        todo!()
    }
}

#[derive(Debug)]
pub struct BenchifyResults {
    // test -> (executor -> timing)
    results: HashMap<String, HashMap<String, std::time::Duration>>,
}

fn main() -> Result<()> {
    color_eyre::install()?;
    pretty_env_logger::init();

    let opts = CmdLineOpts::parse();

    let config: BenchifyConfig = toml::from_str(
        &std::fs::read_to_string(&opts.benchify_toml)
            .or(Err(eyre!("Could not read {:?}", &opts.benchify_toml)))?,
    )?;

    let results = config.execute()?;

    println!("{:?}", results);

    Ok(())
}
