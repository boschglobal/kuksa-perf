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

use crate::providers::kuksa_val_v1::provider as kuksa_val_v1;
use crate::providers::kuksa_val_v2::provider as kuksa_val_v2;
use crate::providers::provider_trait::{ProviderInterface, PublishError};
use crate::providers::sdv_databroker_v1::provider as sdv_databroker_v1;

use crate::config::{Group, Signal};

use crate::shutdown::ShutdownHandler;
use crate::subscriber::{self, Subscriber};
use crate::utils::{write_global_output, write_output};
use anyhow::{Context, Result};
use hdrhistogram::Histogram;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use log::error;
use std::fmt;
use std::sync::Arc;
use std::{
    sync::atomic::Ordering,
    time::{Duration, SystemTime},
};
use tokio::sync::RwLock;
use tokio::{select, task::JoinSet, time::Instant};
use tonic::transport::Endpoint;

#[derive(Clone, PartialEq)]
pub enum Api {
    KuksaValV1,
    KuksaValV2,
    SdvDatabrokerV1,
}

pub struct Provider {
    pub provider_interface: Box<dyn ProviderInterface>,
}

#[derive(Clone)]
pub struct MeasurementConfig {
    pub host: String,
    pub port: u64,
    pub run_seconds: u64,
    pub interval: u16,
    pub skip_seconds: u64,
    pub api: Api,
    pub run_forever: bool,
    pub detailed_output: bool,
}

pub struct MeasurementContext {
    pub measurement_config: MeasurementConfig,
    pub group_name: String,
    pub shutdown_handler: Arc<RwLock<ShutdownHandler>>,
    pub provider: Provider,
    pub signals: Vec<Signal>,
    pub subscriber: Subscriber,
    pub progress: ProgressBar,
    pub hist: Histogram<u64>,
    pub running_hist: Histogram<u64>,
}

pub struct MeasurementResult {
    pub measurement_context: MeasurementContext,
    pub iterations_executed: u64,
    pub signals_skipped: u64,
    pub start_time: SystemTime,
}

impl fmt::Display for Api {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Api::KuksaValV1 => write!(f, "kuksa.val.v1"),
            Api::KuksaValV2 => write!(f, "kuksa.val.v2"),
            Api::SdvDatabrokerV1 => write!(f, "sdv.databroker.v1"),
        }
    }
}

async fn setup_subscriber(
    endpoint: &Endpoint,
    signals: Vec<Signal>,
    api: &Api,
) -> Result<Subscriber> {
    let subscriber_channel = endpoint.connect().await.with_context(|| {
        let host = endpoint.uri().host().unwrap_or("unknown host");
        let port = endpoint
            .uri()
            .port()
            .map_or("unknown port".to_string(), |p| p.to_string());
        format!("Failed to connect to server {}:{}", host, port)
    })?;

    let subscriber = subscriber::Subscriber::new(subscriber_channel, signals, api).await?;

    Ok(subscriber)
}

fn create_databroker_endpoint(host: String, port: u64) -> Result<Endpoint> {
    let databroker_address = format!("{}:{}", host, port);

    let endpoint = tonic::transport::Channel::from_shared(databroker_address.clone())
        .with_context(|| "Failed to parse server url")?;

    // Leave out for now until we decide what and how to configure it
    // let endpoint = endpoint
    //     .initial_stream_window_size(1000 * 3 * 128 * 1024) // 20 MB stream window size
    //     .initial_connection_window_size(1000 * 3 * 128 * 1024) // 20 MB connection window size
    //     .keep_alive_timeout(Duration::from_secs(1)) // 60 seconds keepalive time
    //     .keep_alive_timeout(Duration::from_secs(1)) // 20 seconds keepalive timeout
    //     .timeout(Duration::from_secs(1));

    Ok(endpoint)
}

async fn create_provider(endpoint: &Endpoint, api: &Api) -> Result<Provider> {
    let channel = endpoint.connect().await.with_context(|| {
        let host = endpoint.uri().host().unwrap_or("unknown host");
        let port = endpoint
            .uri()
            .port()
            .map_or("unknown port".to_string(), |p| p.to_string());
        format!("Failed to connect to server {}:{}", host, port)
    })?;

    if *api == Api::KuksaValV2 {
        let provider =
            kuksa_val_v2::Provider::new(channel).with_context(|| "Failed to setup provider")?;
        Ok(Provider {
            provider_interface: Box::new(provider),
        })
    } else if *api == Api::SdvDatabrokerV1 {
        let provider = sdv_databroker_v1::Provider::new(channel)
            .with_context(|| "Failed to setup provider")?;
        Ok(Provider {
            provider_interface: Box::new(provider),
        })
    } else {
        let provider =
            kuksa_val_v1::Provider::new(channel).with_context(|| "Failed to setup provider")?;
        Ok(Provider {
            provider_interface: Box::new(provider),
        })
    }
}

pub async fn perform_measurement(
    measurement_config: MeasurementConfig,
    config_groups: Vec<Group>,
    shutdown_handler: ShutdownHandler,
) -> Result<()> {
    let shutdown_handler_ref = Arc::new(RwLock::new(shutdown_handler));

    let mut tasks: JoinSet<Result<MeasurementResult>> = JoinSet::new();

    let multi_progress_bar = MultiProgress::new();

    for group in config_groups.clone() {
        let shutdown_handler = Arc::clone(&shutdown_handler_ref);

        let provider_endpoint =
            create_databroker_endpoint(measurement_config.host.clone(), measurement_config.port)?;
        let subscriber_endpoint =
            create_databroker_endpoint(measurement_config.host.clone(), measurement_config.port)?;
        let mut measurement_config = measurement_config.clone();

        measurement_config.interval = group.cycle_time_ms;
        let mut provider = create_provider(&provider_endpoint, &measurement_config.api).await?;

        let signals = provider
            .provider_interface
            .as_mut()
            .validate_signals_metadata(&group.signals)
            .await
            .unwrap();

        let subscriber = setup_subscriber(
            &subscriber_endpoint,
            signals.clone(),
            &measurement_config.api,
        )
        .await?;

        let hist = Histogram::<u64>::new_with_bounds(1, 60 * 60 * 1000 * 1000, 3)?;
        let running_hist = Histogram::<u64>::new_with_bounds(1, 60 * 60 * 1000 * 1000, 3)?;

        let start_time = SystemTime::now();

        let progress = if measurement_config.run_forever {
            ProgressBar::new_spinner().with_style(
                // TODO: Add average latency etc...
                ProgressStyle::with_template("[{elapsed_precise}] {wide_msg} {pos:>7} seconds")
                    .unwrap(),
            )
        } else {
            ProgressBar::new(measurement_config.run_seconds).with_style(
                ProgressStyle::with_template(
                    "[{elapsed_precise}] {msg} [{wide_bar}] {pos:>7}/{len:7} seconds",
                )
                .unwrap()
                .progress_chars("=> "),
            )
        };
        multi_progress_bar.add(progress.clone());

        let group_name = group.group_name.clone();

        let mut measurement_context = MeasurementContext {
            measurement_config,
            group_name: group_name.clone(),
            shutdown_handler,
            provider,
            signals,
            subscriber,
            progress,
            hist,
            running_hist,
        };

        tasks.spawn(async move {
            let (iterations_executed, signals_skipped) =
                measurement_loop(&mut measurement_context).await.unwrap();

            measurement_context.progress.finish();

            Ok(MeasurementResult {
                measurement_context,
                iterations_executed,
                signals_skipped,
                start_time,
            })
        });
    }

    let mut measurements_results = Vec::<MeasurementResult>::new();
    while let Some(received) = tasks.join_next().await {
        match received {
            Ok(Ok(measurement_result)) => {
                // Whenever the first measurement result is received it should stop the execution of the remaning running groups.
                measurement_result
                    .measurement_context
                    .shutdown_handler
                    .write()
                    .await
                    .state
                    .running
                    .store(false, Ordering::SeqCst);

                measurements_results.push(measurement_result);
            }
            Ok(Err(err)) => {
                error!("{}", err.to_string());
                break;
            }
            Err(err) => {
                error!("{}", err.to_string());
                break;
            }
        }
    }

    let _ = write_global_output(&measurement_config, &measurements_results);

    if measurement_config.detailed_output {
        for group in config_groups {
            let measurement_result = measurements_results
                .iter()
                .find(|result| result.measurement_context.group_name == group.group_name)
                .unwrap();

            write_output(measurement_result).unwrap();
        }
    }
    Ok(())
}

async fn measurement_loop(ctx: &mut MeasurementContext) -> Result<(u64, u64)> {
    let mut iterations = 0;
    let mut skipped = 0;
    let mut last_running_hist = Instant::now();
    let start_run = Instant::now();

    let run_milliseconds = ctx.measurement_config.run_seconds * 1000;
    let skip_milliseconds = ctx.measurement_config.skip_seconds * 1000;

    let mut interval_to_run = if ctx.measurement_config.interval == 0 {
        None
    } else {
        Some(tokio::time::interval(Duration::from_millis(
            ctx.measurement_config.interval.into(),
        )))
    };

    loop {
        if !ctx.measurement_config.run_forever
            && start_run.elapsed().as_millis() >= run_milliseconds.into()
            || !ctx
                .shutdown_handler
                .read()
                .await
                .state
                .running
                .load(Ordering::SeqCst)
        {
            break;
        }

        if last_running_hist.elapsed().as_millis() >= 500 {
            ctx.progress.set_message(format!(
                "Group: {} | Cycle(ms): {} | Current latency: {:.3} ms",
                ctx.group_name,
                ctx.measurement_config.interval,
                ctx.running_hist.mean() / 1000.
            ));
            ctx.running_hist.reset();
            last_running_hist = Instant::now();
        }

        if let Some(interval_to_run) = interval_to_run.as_mut() {
            interval_to_run.tick().await;
        }

        let provider = ctx.provider.provider_interface.as_ref();
        let publish_task = provider.publish(&ctx.signals, iterations);

        let mut subscriber_tasks: JoinSet<Result<Instant, subscriber::Error>> = JoinSet::new();

        for signal in &ctx.signals {
            // TODO: return an awaitable thingie (wrapping the Receiver<Instant>)
            let mut sub = ctx.subscriber.wait_for(&signal.path)?;
            let mut shutdown_triggered = ctx.shutdown_handler.write().await.trigger.subscribe();

            subscriber_tasks.spawn(async move {
                // Wait for notification or shutdown
                select! {
                    instant = sub.recv() => {
                        instant.map_err(|err| subscriber::Error::RecvFailed(err.to_string()))
                    }
                    _ = shutdown_triggered.recv() => {
                        Err(subscriber::Error::Shutdown)
                    }
                }
            });
        }

        let published = {
            let mut shutdown_triggered = ctx.shutdown_handler.write().await.trigger.subscribe();
            select! {
                published = publish_task => published,
                _ = shutdown_triggered.recv() => {
                    Err(PublishError::Shutdown)
                }
            }
        }?;

        while let Some(received) = subscriber_tasks.join_next().await {
            match received {
                Ok(Ok(received)) => {
                    if start_run.elapsed().as_millis() < skip_milliseconds.into() {
                        skipped += 1;
                        continue;
                    }
                    let latency = received
                        .duration_since(published)
                        .as_micros()
                        .try_into()
                        .unwrap();
                    ctx.hist.record(latency)?;
                    ctx.running_hist.record(latency)?;
                }
                Ok(Err(subscriber::Error::Shutdown)) => {
                    break;
                }
                Ok(Err(err)) => {
                    error!("{}", err.to_string());
                    break;
                }
                Err(err) => {
                    error!("{}", err.to_string());
                    break;
                }
            }
        }

        iterations += 1;
        ctx.progress.set_position(start_run.elapsed().as_secs());
    }
    Ok((iterations, skipped))
}
