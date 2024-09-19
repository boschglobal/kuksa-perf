/********************************************************************************
* Copyright (c) 2024 Contributors to the Eclipse Foundation
*
* See the NOTICE file(s) distributed with this work for additional
* information regarding copyright ownership.
*
* This program and the accompanying materials are made available under the
* terms of the Apache License 2.0 which is available at
* http://www.apache.org/licenses/LICENSE-2.0
*
* SPDX-License-Identifier: Apache-2.0
********************************************************************************/

use anyhow::Result;
use clap::Parser;
use measure::{perform_measurement, Api, MeasurementConfig};
use shutdown::setup_shutdown_handler;
use std::cmp::{max, min};

use utils::read_config;

mod config;
mod measure;
mod providers;
mod shutdown;
mod subscriber;
mod utils;

#[derive(Parser)]
#[clap(author, version, about)]
struct Args {
    /// Number of iterations to run.
    #[clap(
        long,
        short,
        display_order = 1,
        default_value_t = 1000,
        conflicts_with = "run_forever"
    )]
    iterations: u64,

    /// Api of databroker.
    #[clap(long, display_order = 2, default_value = "kuksa.val.v1", value_parser = clap::builder::PossibleValuesParser::new(["kuksa.val.v1", "kuksa.val.v2", "sdv.databroker.v1"]))]
    api: String,

    /// Host address of databroker.
    #[clap(long, display_order = 3, default_value = "http://127.0.0.1")]
    host: String,

    /// Port of databroker.
    #[clap(long, display_order = 4, default_value_t = 55555)]
    port: u64,

    /// Number of iterations to run (skip) before measuring the latency.
    #[clap(
        long,
        display_order = 5,
        value_name = "ITERATIONS",
        default_value_t = 10
    )]
    skip: u64,

    /// Print more details in the summary result
    #[clap(
        long,
        display_order = 6,
        value_name = "Detailed ouput result",
        default_value_t = false
    )]
    detail_output: bool,

    /// Path to configuration file
    #[clap(long = "config", display_order = 7, value_name = "FILE")]
    config_file: Option<String>,

    /// Run the measurements forever (until receiving a shutdown signal).
    #[clap(
        long,
        action = clap::ArgAction::SetTrue,
        display_order = 8,
        conflicts_with = "iterations"
    )]
    run_forever: bool,

    /// Verbosity level. Can be one of ERROR, WARN, INFO, DEBUG, TRACE.
    #[clap(
        long = "verbosity",
        short,
        display_order = 10,
        value_name = "LEVEL",
        default_value_t = log::Level::Warn
    )]
    verbosity_level: log::Level,
}

fn setup_logging(verbosity_level: log::Level) -> Result<()> {
    stderrlog::new()
        .module(module_path!())
        .verbosity(verbosity_level)
        .init()?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    setup_logging(args.verbosity_level)?;

    let shutdown_handler = setup_shutdown_handler();

    let mut api = Api::KuksaValV1;
    if args.api.contains("sdv.databroker.v1") {
        api = Api::SdvDatabrokerV1;
    } else if args.api.contains("kuksa.val.v2") {
        api = Api::KuksaValV2;
    }

    let config_groups = read_config(args.config_file.as_ref())?;

    // Skip at most _iterations_ number of iterations
    let skip = max(0, min(args.iterations, args.skip));

    let measurement_config = MeasurementConfig {
        host: args.host,
        port: args.port,
        iterations: args.iterations,
        interval: 0,
        skip,
        api,
        run_forever: args.run_forever,
        detail_output: args.detail_output,
    };

    perform_measurement(measurement_config, config_groups, shutdown_handler).await?;
    Ok(())
}
