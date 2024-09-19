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

use anyhow::{anyhow, Context, Ok, Result};
use console::Term;
use log::debug;
use serde_json::from_reader;
use std::{cmp::max, fs::OpenOptions, io::Write, time::SystemTime};

use crate::{
    config::{Config, Group, Signal},
    measure::MeasurementResult,
};

const MAX_NUMBER_OF_GROUPS: usize = 10;

pub fn read_config(config_file: Option<&String>) -> Result<Vec<Group>> {
    match config_file {
        Some(filename) => {
            let file = OpenOptions::new()
                .read(true)
                .open(filename)
                .with_context(|| format!("Failed to open configuration file '{}'", filename))?;
            let config: Config = from_reader(file)
                .with_context(|| format!("Failed to parse configuration file '{}'", filename))?;

            if config.groups.len() > MAX_NUMBER_OF_GROUPS {
                return Err(anyhow!(
                    "The number of groups exceeds the maximum allowed limit of {}",
                    MAX_NUMBER_OF_GROUPS
                ));
            }

            Ok(config.groups)
        }
        None => {
            // Return a default set of groups or handle the None case appropriately
            Ok(vec![
                Group {
                    group_name: String::from("Frame A"),
                    cycle_time_ms: 10,
                    signals: vec![Signal {
                        path: String::from("Vehicle.Speed"),
                    }],
                },
                Group {
                    group_name: String::from("Frame B"),
                    cycle_time_ms: 20,
                    signals: vec![Signal {
                        path: String::from("Vehicle.IsBrokenDown"),
                    }],
                },
                Group {
                    group_name: String::from("Frame C"),
                    cycle_time_ms: 30,
                    signals: vec![Signal {
                        path: String::from("Vehicle.Body.Windshield.Front.Wiping.Intensity"),
                    }],
                },
            ])
        }
    }
}

pub fn write_output(measurement_result: &MeasurementResult) -> Result<()> {
    let mut stdout = Term::stdout();
    let end_time = SystemTime::now();
    let total_duration = end_time.duration_since(measurement_result.start_time)?;
    let measurement_context = &measurement_result.measurement_context;
    let measurement_config = &measurement_context.measurement_config;

    writeln!(
        stdout,
        "\n\nGroup: {} | Cycle time(ms): {}",
        measurement_context.group_name, measurement_config.interval
    )?;

    if !measurement_config.detail_output {
        writeln!(
            stdout,
            "  Average:   {:>7.3} ms",
            measurement_context.hist.mean() / 1000.0
        )?;
        writeln!(
            stdout,
            "  95% in under {:.3} ms",
            measurement_context.hist.value_at_quantile(95_f64 / 100.0) as f64 / 1000.0
        )?;
    } else {
        writeln!(
            stdout,
            "  Elapsed time: {:.2} s",
            total_duration.as_millis() as f64 / 1000.0
        )?;
        let rate_limit = match measurement_config.interval {
            0 => "None".into(),
            ms => format!("{} ms between iterations", ms),
        };
        writeln!(stdout, "  Rate limit: {}", rate_limit)?;
        writeln!(
            stdout,
            "  Sent: {} iterations * {} signals = {} updates",
            measurement_result.iterations_executed,
            measurement_context.signals.len(),
            measurement_result.iterations_executed * measurement_context.signals.len() as u64
        )?;
        writeln!(
            stdout,
            "  Skipped: {} updates",
            measurement_result.iterations_skipped
        )?;
        writeln!(
            stdout,
            "  Received: {} updates",
            measurement_context.hist.len()
        )?;
        writeln!(
            stdout,
            "  Fastest: {:>7.3} ms",
            measurement_context.hist.min() as f64 / 1000.0
        )?;
        writeln!(
            stdout,
            "  Slowest: {:>7.3} ms",
            measurement_context.hist.max() as f64 / 1000.0
        )?;
        writeln!(
            stdout,
            "  Average: {:>7.3} ms",
            measurement_context.hist.mean() / 1000.0
        )?;

        writeln!(stdout, "\nLatency histogram:")?;

        let step_size = max(
            1,
            (measurement_context.hist.max() - measurement_context.hist.min()) / 11,
        );

        let buckets = measurement_context.hist.iter_linear(step_size);

        // skip initial empty buckets
        let buckets = buckets.skip_while(|v| v.count_since_last_iteration() == 0);

        let mut histogram = Vec::with_capacity(11);

        for v in buckets {
            let mean = v.value_iterated_to() + 1 - step_size / 2; // +1 to make range inclusive
            let count = v.count_since_last_iteration();
            histogram.push((mean, count));
        }

        let (_, cols) = stdout.size();
        debug!("Number of columns: {cols}");

        for (mean, count) in histogram {
            let bars = count as f64
                / (measurement_context.hist.len() * measurement_context.signals.len() as u64)
                    as f64
                * (cols - 22) as f64;
            let bar = "∎".repeat(bars as usize);
            writeln!(
                stdout,
                "  {:>7.3} ms [{:<5}] |{}",
                mean as f64 / 1000.0,
                count,
                bar
            )?;
        }

        writeln!(stdout, "\nLatency distribution:")?;

        for q in &[10, 25, 50, 75, 90, 95, 99] {
            writeln!(
                stdout,
                "  {q}% in under {:.3} ms",
                measurement_context
                    .hist
                    .value_at_quantile(*q as f64 / 100.0) as f64
                    / 1000.0
            )?;
        }
    }
    Ok(())
}
