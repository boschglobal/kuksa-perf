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

use crate::config::Signal;
use crate::providers::provider_trait::{Error, ProviderInterface, PublishError};
use crate::types::DataValue;
use databroker_proto::sdv::databroker::v1 as proto;
use tokio_stream::wrappers::ReceiverStream;

use tonic::async_trait;

use log::{debug, error};

use tokio::{
    sync::mpsc::{self, Receiver, Sender},
    time::Instant,
};

use std::collections::HashMap;
use std::convert::TryFrom;
use tonic::transport::Channel;

pub struct Provider {
    tx: Sender<proto::StreamDatapointsRequest>,
    metadata: HashMap<String, proto::Metadata>,
    id_to_path: HashMap<i32, String>,
    channel: Channel,
    initial_signals_values: HashMap<Signal, DataValue>,
}

impl Provider {
    pub fn new(channel: Channel) -> Result<Self, Error> {
        let (tx, rx) = mpsc::channel(10);

        tokio::spawn(Provider::run(rx, channel.clone()));
        Ok(Provider {
            tx,
            metadata: HashMap::new(),
            id_to_path: HashMap::new(),
            channel,
            initial_signals_values: HashMap::new(),
        })
    }

    async fn run(
        rx: Receiver<proto::StreamDatapointsRequest>,
        channel: Channel,
    ) -> Result<(), Error> {
        let mut client = proto::collector_client::CollectorClient::new(channel);

        match client.stream_datapoints(ReceiverStream::new(rx)).await {
            Ok(response) => {
                let mut stream = response.into_inner();

                while let Ok(message) = stream.message().await {
                    match message {
                        Some(message) => {
                            for error in message.errors {
                                let error_code = proto::DatapointError::try_from(error.1)
                                    .unwrap()
                                    .as_str_name();
                                let id = error.0;
                                error!("{}: error setting datapoint {}", error_code, id)
                            }
                        }
                        None => {
                            debug!("stream to databroker closed");
                            break;
                        }
                    }
                }
            }
            Err(err) => {
                error!("failed to setup provider stream: {}", err.message());
            }
        }
        debug!("provider::run() exiting");
        Ok(())
    }
}

#[async_trait]
impl ProviderInterface for Provider {
    async fn publish(
        &self,
        signal_data: &[Signal],
        iteration: u64,
    ) -> Result<Instant, PublishError> {
        let datapoints = HashMap::from_iter(signal_data.iter().map(|path: &Signal| {
            let metadata = self.metadata.get(&path.path).unwrap();
            (
                metadata.id,
                proto::Datapoint {
                    timestamp: None,
                    value: Some(n_to_value(metadata, iteration).unwrap()),
                },
            )
        }));

        let payload = proto::StreamDatapointsRequest { datapoints };

        let now = Instant::now();
        self.tx
            .send(payload)
            .await
            .map_err(|err| PublishError::SendFailure(err.to_string()))?;
        Ok(now)
    }

    async fn validate_signals_metadata(
        &mut self,
        signals: &[Signal],
    ) -> Result<Vec<Signal>, Error> {
        let signals: Vec<String> = signals.iter().map(|signal| signal.path.clone()).collect();
        let number_of_signals = signals.len();
        let mut client = proto::broker_client::BrokerClient::new(self.channel.clone())
            .max_decoding_message_size(64 * 1024 * 1024)
            .max_encoding_message_size(64 * 1024 * 1024);

        let response = client
            .get_metadata(tonic::Request::new(proto::GetMetadataRequest {
                names: signals.clone(),
            }))
            .await
            .map_err(|err| Error::MetadataError(format!("failed to fetch metadata: {}", err)))?;

        let signals_response: Vec<Signal> = response
            .into_inner()
            .list
            .into_iter()
            .map(|metadata| {
                self.metadata
                    .insert(metadata.name.clone(), metadata.clone());
                self.id_to_path.insert(metadata.id, metadata.name.clone());
                Signal {
                    path: metadata.name,
                    id: Some(metadata.id),
                }
            })
            .collect();

        debug!(
            "received {} number of signals in metadata",
            self.metadata.len()
        );
        if self.metadata.len() < number_of_signals {
            let missing_signals: Vec<_> = signals
                .iter()
                .filter(|signal| !self.metadata.contains_key(signal.as_str()))
                .collect();

            Err(Error::MetadataError(format!(
                "The following signals are missing in the databroker: {:?}",
                missing_signals
            )))
        } else {
            Ok(signals_response)
        }
    }

    async fn set_initial_signals_values(
        &mut self,
        initial_signals_values: HashMap<Signal, DataValue>,
    ) -> Result<(), Error> {
        self.initial_signals_values = initial_signals_values;
        Ok(())
    }
}

pub fn n_to_value(
    metadata: &proto::Metadata,
    n: u64,
) -> Result<proto::datapoint::Value, PublishError> {
    match proto::DataType::try_from(metadata.data_type) {
        Ok(proto::DataType::String) => match &metadata.value_restriction {
            Some(value_restriction) => match &value_restriction.r#type {
                Some(proto::value_restriction::Type::String(values)) => {
                    let index = n % values.allowed_values.len() as u64;
                    let value = values.allowed_values[index as usize].clone();
                    Ok(proto::datapoint::Value::StringValue(value))
                }
                _ => Err(PublishError::MetadataError),
            },
            None => Ok(proto::datapoint::Value::StringValue(n.to_string())),
        },
        Ok(proto::DataType::Bool) => match n % 2 {
            0 => Ok(proto::datapoint::Value::BoolValue(true)),
            _ => Ok(proto::datapoint::Value::BoolValue(false)),
        },
        Ok(proto::DataType::Int8) => match &metadata.value_restriction {
            Some(value_restriction) => match &value_restriction.r#type {
                Some(proto::value_restriction::Type::Signed(values)) => {
                    if values.max.is_some() || values.min.is_some() {
                        let min = values.min.unwrap_or(i8::MIN.into());
                        let max = values.max.unwrap_or(i8::MAX.into());
                        let mut value = min + n as i64;
                        if value > max {
                            value %= max;
                            if value < min {
                                value = min;
                            }
                        }
                        Ok(proto::datapoint::Value::Int32Value(value as i32))
                    } else if !values.allowed_values.is_empty() {
                        let index = n % values.allowed_values.len() as u64;
                        let value = values.allowed_values[index as usize];
                        Ok(proto::datapoint::Value::Int32Value(value as i32))
                    } else {
                        Ok(proto::datapoint::Value::Int32Value((n % 128) as i32))
                    }
                }
                _ => Err(PublishError::MetadataError),
            },
            None => Ok(proto::datapoint::Value::Int32Value((n % 128) as i32)),
        },
        Ok(proto::DataType::Int16) => match &metadata.value_restriction {
            Some(value_restriction) => match &value_restriction.r#type {
                Some(proto::value_restriction::Type::Signed(values)) => {
                    if values.max.is_some() || values.min.is_some() {
                        let min = values.min.unwrap_or(i16::MIN.into());
                        let max = values.max.unwrap_or(i16::MAX.into());
                        let mut value = min + n as i64;
                        if value > max {
                            value %= max;
                            if value < min {
                                value = min;
                            }
                        }
                        Ok(proto::datapoint::Value::Int32Value(value as i32))
                    } else if !values.allowed_values.is_empty() {
                        let index = n % values.allowed_values.len() as u64;
                        let value = values.allowed_values[index as usize];
                        Ok(proto::datapoint::Value::Int32Value(value as i32))
                    } else {
                        Ok(proto::datapoint::Value::Int32Value((n % 128) as i32))
                    }
                }
                _ => Err(PublishError::MetadataError),
            },
            None => Ok(proto::datapoint::Value::Int32Value((n % 128) as i32)),
        },
        Ok(proto::DataType::Int32) => match &metadata.value_restriction {
            Some(value_restriction) => match &value_restriction.r#type {
                Some(proto::value_restriction::Type::Signed(values)) => {
                    if values.max.is_some() || values.min.is_some() {
                        let min = values.min.unwrap_or(i32::MIN.into());
                        let max = values.max.unwrap_or(i32::MAX.into());
                        let mut value = min + n as i64;
                        if value > max {
                            value %= max;
                            if value < min {
                                value = min;
                            }
                        }
                        Ok(proto::datapoint::Value::Int32Value(value as i32))
                    } else if !values.allowed_values.is_empty() {
                        let index = n % values.allowed_values.len() as u64;
                        let value = values.allowed_values[index as usize];
                        Ok(proto::datapoint::Value::Int32Value(value as i32))
                    } else {
                        Ok(proto::datapoint::Value::Int32Value((n % 128) as i32))
                    }
                }
                _ => Err(PublishError::MetadataError),
            },
            None => Ok(proto::datapoint::Value::Int32Value((n % 128) as i32)),
        },
        Ok(proto::DataType::Int64) => match &metadata.value_restriction {
            Some(value_restriction) => match &value_restriction.r#type {
                Some(proto::value_restriction::Type::Signed(values)) => {
                    if values.max.is_some() || values.min.is_some() {
                        let min = values.min.unwrap_or(i64::MIN);
                        let max = values.max.unwrap_or(i64::MAX);
                        let mut value = min + n as i64;
                        if value > max {
                            value %= max;
                            if value < min {
                                value = min;
                            }
                        }
                        Ok(proto::datapoint::Value::Int64Value(value))
                    } else if !values.allowed_values.is_empty() {
                        let index = n % values.allowed_values.len() as u64;
                        let value = values.allowed_values[index as usize];
                        Ok(proto::datapoint::Value::Int64Value(value))
                    } else {
                        Ok(proto::datapoint::Value::Int64Value((n % 128) as i64))
                    }
                }
                _ => Err(PublishError::MetadataError),
            },
            None => Ok(proto::datapoint::Value::Int64Value((n % 128) as i64)),
        },
        Ok(proto::DataType::Uint8) => match &metadata.value_restriction {
            Some(value_restriction) => match &value_restriction.r#type {
                Some(proto::value_restriction::Type::Unsigned(values)) => {
                    if values.max.is_some() || values.min.is_some() {
                        let min = values.min.unwrap_or(u8::MIN.into());
                        let max = values.max.unwrap_or(u8::MAX.into());
                        let mut value = min + n;
                        if value > max {
                            value %= max;
                            if value < min {
                                value = min;
                            }
                        }
                        Ok(proto::datapoint::Value::Uint32Value(value as u32))
                    } else if !values.allowed_values.is_empty() {
                        let index = n % values.allowed_values.len() as u64;
                        let value = values.allowed_values[index as usize];
                        Ok(proto::datapoint::Value::Uint32Value(value as u32))
                    } else {
                        Ok(proto::datapoint::Value::Uint32Value((n % 128) as u32))
                    }
                }
                _ => Err(PublishError::MetadataError),
            },
            None => Ok(proto::datapoint::Value::Uint32Value((n % 128) as u32)),
        },
        Ok(proto::DataType::Uint16) => match &metadata.value_restriction {
            Some(value_restriction) => match &value_restriction.r#type {
                Some(proto::value_restriction::Type::Unsigned(values)) => {
                    if values.max.is_some() || values.min.is_some() {
                        let min = values.min.unwrap_or(u16::MIN.into());
                        let max = values.max.unwrap_or(u16::MAX.into());
                        let mut value = min + n;
                        if value > max {
                            value %= max;
                            if value < min {
                                value = min;
                            }
                        }
                        Ok(proto::datapoint::Value::Uint32Value(value as u32))
                    } else if !values.allowed_values.is_empty() {
                        let index = n % values.allowed_values.len() as u64;
                        let value = values.allowed_values[index as usize];
                        Ok(proto::datapoint::Value::Uint32Value(value as u32))
                    } else {
                        Ok(proto::datapoint::Value::Uint32Value((n % 128) as u32))
                    }
                }
                _ => Err(PublishError::MetadataError),
            },
            None => Ok(proto::datapoint::Value::Uint32Value((n % 128) as u32)),
        },
        Ok(proto::DataType::Uint32) => match &metadata.value_restriction {
            Some(value_restriction) => match &value_restriction.r#type {
                Some(proto::value_restriction::Type::Unsigned(values)) => {
                    if values.max.is_some() || values.min.is_some() {
                        let min = values.min.unwrap_or(u32::MIN.into());
                        let max = values.max.unwrap_or(u32::MAX.into());
                        let mut value = min + n;
                        if value > max {
                            value %= max;
                            if value < min {
                                value = min;
                            }
                        }
                        Ok(proto::datapoint::Value::Uint32Value(value as u32))
                    } else if !values.allowed_values.is_empty() {
                        let index = n % values.allowed_values.len() as u64;
                        let value = values.allowed_values[index as usize];
                        Ok(proto::datapoint::Value::Uint32Value(value as u32))
                    } else {
                        Ok(proto::datapoint::Value::Uint32Value((n % 128) as u32))
                    }
                }
                _ => Err(PublishError::MetadataError),
            },
            None => Ok(proto::datapoint::Value::Uint32Value((n % 128) as u32)),
        },
        Ok(proto::DataType::Uint64) => match &metadata.value_restriction {
            Some(value_restriction) => match &value_restriction.r#type {
                Some(proto::value_restriction::Type::Unsigned(values)) => {
                    if values.max.is_some() || values.min.is_some() {
                        let min = values.min.unwrap_or(u64::MIN);
                        let max = values.max.unwrap_or(u64::MAX);
                        let mut value = min + n;
                        if value > max {
                            value %= max;
                            if value < min {
                                value = min;
                            }
                        }
                        Ok(proto::datapoint::Value::Uint64Value(value))
                    } else if !values.allowed_values.is_empty() {
                        let index = n % values.allowed_values.len() as u64;
                        let value = values.allowed_values[index as usize];
                        Ok(proto::datapoint::Value::Uint64Value(value))
                    } else {
                        Ok(proto::datapoint::Value::Uint64Value(n % 128))
                    }
                }
                _ => Err(PublishError::MetadataError),
            },
            None => Ok(proto::datapoint::Value::Uint64Value(n % 128)),
        },
        Ok(proto::DataType::Float) => match &metadata.value_restriction {
            Some(value_restriction) => match &value_restriction.r#type {
                Some(proto::value_restriction::Type::FloatingPoint(values)) => {
                    if values.max.is_some() || values.min.is_some() {
                        let min = values.min.unwrap_or(f32::MIN.into()) as f32;
                        let max = values.max.unwrap_or(f32::MAX.into()) as f32;
                        let mut value = min + n as f32;
                        if value > max {
                            value %= max;
                            if value < min {
                                value = min;
                            }
                        }
                        Ok(proto::datapoint::Value::FloatValue(value))
                    } else if !values.allowed_values.is_empty() {
                        let index = n % values.allowed_values.len() as u64;
                        let value = values.allowed_values[index as usize] as f32;
                        Ok(proto::datapoint::Value::FloatValue(value))
                    } else {
                        Ok(proto::datapoint::Value::FloatValue(n as f32))
                    }
                }
                _ => Err(PublishError::MetadataError),
            },
            None => Ok(proto::datapoint::Value::FloatValue(n as f32)),
        },
        Ok(proto::DataType::Double) => match &metadata.value_restriction {
            Some(value_restriction) => match &value_restriction.r#type {
                Some(proto::value_restriction::Type::FloatingPoint(values)) => {
                    if values.max.is_some() || values.min.is_some() {
                        let min = values.min.unwrap_or(f32::MIN.into()) as f32;
                        let max = values.max.unwrap_or(f32::MAX.into()) as f32;
                        let mut value = min + n as f32;
                        if value > max {
                            value %= max;
                            if value < min {
                                value = min;
                            }
                        }
                        Ok(proto::datapoint::Value::DoubleValue(value as f64))
                    } else if !values.allowed_values.is_empty() {
                        let index = n % values.allowed_values.len() as u64;
                        let value = values.allowed_values[index as usize] as f32;
                        Ok(proto::datapoint::Value::DoubleValue(value as f64))
                    } else {
                        Ok(proto::datapoint::Value::DoubleValue(n as f64))
                    }
                }
                _ => Err(PublishError::MetadataError),
            },
            None => Ok(proto::datapoint::Value::DoubleValue(n as f64)),
        },
        Ok(proto::DataType::StringArray) => match &metadata.value_restriction {
            Some(value_restriction) => match &value_restriction.r#type {
                Some(proto::value_restriction::Type::String(values)) => {
                    let index = n % values.allowed_values.len() as u64;
                    let value = values.allowed_values[index as usize].clone();
                    Ok(proto::datapoint::Value::StringArray(proto::StringArray {
                        values: vec![value],
                    }))
                }
                _ => Err(PublishError::MetadataError),
            },
            None => Ok(proto::datapoint::Value::StringArray(proto::StringArray {
                values: vec![n.to_string()],
            })),
        },
        Ok(proto::DataType::BoolArray) => {
            Ok(proto::datapoint::Value::BoolArray(proto::BoolArray {
                values: vec![matches!(n % 2, 0)],
            }))
        }
        Ok(proto::DataType::Int8Array) => match &metadata.value_restriction {
            Some(value_restriction) => match &value_restriction.r#type {
                Some(proto::value_restriction::Type::Signed(values)) => {
                    if values.max.is_some() || values.min.is_some() {
                        let min = values.min.unwrap_or(i8::MIN.into());
                        let max = values.max.unwrap_or(i8::MAX.into());
                        let mut value = min + n as i64;
                        if value > max {
                            value %= max;
                            if value < min {
                                value = min;
                            }
                        }
                        Ok(proto::datapoint::Value::Int32Array(proto::Int32Array {
                            values: vec![value as i32],
                        }))
                    } else if !values.allowed_values.is_empty() {
                        let index = n % values.allowed_values.len() as u64;
                        let value = values.allowed_values[index as usize];
                        Ok(proto::datapoint::Value::Int32Array(proto::Int32Array {
                            values: vec![value as i32],
                        }))
                    } else {
                        Ok(proto::datapoint::Value::Int32Array(proto::Int32Array {
                            values: vec![(n % 128) as i32],
                        }))
                    }
                }
                _ => Err(PublishError::MetadataError),
            },
            None => Ok(proto::datapoint::Value::Int32Array(proto::Int32Array {
                values: vec![(n % 128) as i32],
            })),
        },
        Ok(proto::DataType::Int16Array) => match &metadata.value_restriction {
            Some(value_restriction) => match &value_restriction.r#type {
                Some(proto::value_restriction::Type::Signed(values)) => {
                    if values.max.is_some() || values.min.is_some() {
                        let min = values.min.unwrap_or(i16::MIN.into());
                        let max = values.max.unwrap_or(i16::MAX.into());
                        let mut value = min + n as i64;
                        if value > max {
                            value %= max;
                            if value < min {
                                value = min;
                            }
                        }
                        Ok(proto::datapoint::Value::Int32Array(proto::Int32Array {
                            values: vec![value as i32],
                        }))
                    } else if !values.allowed_values.is_empty() {
                        let index = n % values.allowed_values.len() as u64;
                        let value = values.allowed_values[index as usize];
                        Ok(proto::datapoint::Value::Int32Array(proto::Int32Array {
                            values: vec![value as i32],
                        }))
                    } else {
                        Ok(proto::datapoint::Value::Int32Array(proto::Int32Array {
                            values: vec![(n % 128) as i32],
                        }))
                    }
                }
                _ => Err(PublishError::MetadataError),
            },
            None => Ok(proto::datapoint::Value::Int32Array(proto::Int32Array {
                values: vec![(n % 128) as i32],
            })),
        },
        Ok(proto::DataType::Int32Array) => match &metadata.value_restriction {
            Some(value_restriction) => match &value_restriction.r#type {
                Some(proto::value_restriction::Type::Signed(values)) => {
                    if values.max.is_some() || values.min.is_some() {
                        let min = values.min.unwrap_or(i32::MIN.into());
                        let max = values.max.unwrap_or(i32::MAX.into());
                        let mut value = min + n as i64;
                        if value > max {
                            value %= max;
                            if value < min {
                                value = min;
                            }
                        }
                        Ok(proto::datapoint::Value::Int32Array(proto::Int32Array {
                            values: vec![value as i32],
                        }))
                    } else if !values.allowed_values.is_empty() {
                        let index = n % values.allowed_values.len() as u64;
                        let value = values.allowed_values[index as usize];
                        Ok(proto::datapoint::Value::Int32Array(proto::Int32Array {
                            values: vec![value as i32],
                        }))
                    } else {
                        Ok(proto::datapoint::Value::Int32Array(proto::Int32Array {
                            values: vec![(n % 128) as i32],
                        }))
                    }
                }
                _ => Err(PublishError::MetadataError),
            },
            None => Ok(proto::datapoint::Value::Int32Array(proto::Int32Array {
                values: vec![(n % 128) as i32],
            })),
        },
        Ok(proto::DataType::Int64Array) => match &metadata.value_restriction {
            Some(value_restriction) => match &value_restriction.r#type {
                Some(proto::value_restriction::Type::Signed(values)) => {
                    if values.max.is_some() || values.min.is_some() {
                        let min = values.min.unwrap_or(i64::MIN);
                        let max = values.max.unwrap_or(i64::MAX);
                        let mut value = min + n as i64;
                        if value > max {
                            value %= max;
                            if value < min {
                                value = min;
                            }
                        }
                        Ok(proto::datapoint::Value::Int64Array(proto::Int64Array {
                            values: vec![value],
                        }))
                    } else if !values.allowed_values.is_empty() {
                        let index = n % values.allowed_values.len() as u64;
                        let value = values.allowed_values[index as usize];
                        Ok(proto::datapoint::Value::Int64Array(proto::Int64Array {
                            values: vec![value],
                        }))
                    } else {
                        Ok(proto::datapoint::Value::Int64Array(proto::Int64Array {
                            values: vec![(n % 128) as i64],
                        }))
                    }
                }
                _ => Err(PublishError::MetadataError),
            },
            None => Ok(proto::datapoint::Value::Int64Array(proto::Int64Array {
                values: vec![(n % 128) as i64],
            })),
        },
        Ok(proto::DataType::Uint8Array) => match &metadata.value_restriction {
            Some(value_restriction) => match &value_restriction.r#type {
                Some(proto::value_restriction::Type::Unsigned(values)) => {
                    if values.max.is_some() || values.min.is_some() {
                        let min = values.min.unwrap_or(u8::MIN.into());
                        let max = values.max.unwrap_or(u8::MAX.into());
                        let mut value = min + n;
                        if value > max {
                            value %= max;
                            if value < min {
                                value = min;
                            }
                        }
                        Ok(proto::datapoint::Value::Uint32Array(proto::Uint32Array {
                            values: vec![value as u32],
                        }))
                    } else if !values.allowed_values.is_empty() {
                        let index = n % values.allowed_values.len() as u64;
                        let value = values.allowed_values[index as usize];
                        Ok(proto::datapoint::Value::Uint32Array(proto::Uint32Array {
                            values: vec![value as u32],
                        }))
                    } else {
                        Ok(proto::datapoint::Value::Uint32Array(proto::Uint32Array {
                            values: vec![(n % 128) as u32],
                        }))
                    }
                }
                _ => Err(PublishError::MetadataError),
            },
            None => Ok(proto::datapoint::Value::Uint32Array(proto::Uint32Array {
                values: vec![(n % 128) as u32],
            })),
        },
        Ok(proto::DataType::Uint16Array) => match &metadata.value_restriction {
            Some(value_restriction) => match &value_restriction.r#type {
                Some(proto::value_restriction::Type::Unsigned(values)) => {
                    if values.max.is_some() || values.min.is_some() {
                        let min = values.min.unwrap_or(u16::MIN.into());
                        let max = values.max.unwrap_or(u16::MAX.into());
                        let mut value = min + n;
                        if value > max {
                            value %= max;
                            if value < min {
                                value = min;
                            }
                        }
                        Ok(proto::datapoint::Value::Uint32Array(proto::Uint32Array {
                            values: vec![value as u32],
                        }))
                    } else if !values.allowed_values.is_empty() {
                        let index = n % values.allowed_values.len() as u64;
                        let value = values.allowed_values[index as usize];
                        Ok(proto::datapoint::Value::Uint32Array(proto::Uint32Array {
                            values: vec![value as u32],
                        }))
                    } else {
                        Ok(proto::datapoint::Value::Uint32Array(proto::Uint32Array {
                            values: vec![(n % 128) as u32],
                        }))
                    }
                }
                _ => Err(PublishError::MetadataError),
            },
            None => Ok(proto::datapoint::Value::Uint32Array(proto::Uint32Array {
                values: vec![(n % 128) as u32],
            })),
        },
        Ok(proto::DataType::Uint32Array) => match &metadata.value_restriction {
            Some(value_restriction) => match &value_restriction.r#type {
                Some(proto::value_restriction::Type::Unsigned(values)) => {
                    if values.max.is_some() || values.min.is_some() {
                        let min = values.min.unwrap_or(u32::MIN.into());
                        let max = values.max.unwrap_or(u32::MAX.into());
                        let mut value = min + n;
                        if value > max {
                            value %= max;
                            if value < min {
                                value = min;
                            }
                        }
                        Ok(proto::datapoint::Value::Uint32Array(proto::Uint32Array {
                            values: vec![value as u32],
                        }))
                    } else if !values.allowed_values.is_empty() {
                        let index = n % values.allowed_values.len() as u64;
                        let value = values.allowed_values[index as usize];
                        Ok(proto::datapoint::Value::Uint32Array(proto::Uint32Array {
                            values: vec![value as u32],
                        }))
                    } else {
                        Ok(proto::datapoint::Value::Uint32Array(proto::Uint32Array {
                            values: vec![(n % 128) as u32],
                        }))
                    }
                }
                _ => Err(PublishError::MetadataError),
            },
            None => Ok(proto::datapoint::Value::Uint32Array(proto::Uint32Array {
                values: vec![(n % 128) as u32],
            })),
        },
        Ok(proto::DataType::Uint64Array) => match &metadata.value_restriction {
            Some(value_restriction) => match &value_restriction.r#type {
                Some(proto::value_restriction::Type::Unsigned(values)) => {
                    if values.max.is_some() || values.min.is_some() {
                        let min = values.min.unwrap_or(u64::MIN);
                        let max = values.max.unwrap_or(u64::MAX);
                        let mut value = min + n;
                        if value > max {
                            value %= max;
                            if value < min {
                                value = min;
                            }
                        }
                        Ok(proto::datapoint::Value::Uint64Array(proto::Uint64Array {
                            values: vec![value],
                        }))
                    } else if !values.allowed_values.is_empty() {
                        let index = n % values.allowed_values.len() as u64;
                        let value = values.allowed_values[index as usize];
                        Ok(proto::datapoint::Value::Uint64Array(proto::Uint64Array {
                            values: vec![value],
                        }))
                    } else {
                        Ok(proto::datapoint::Value::Uint64Array(proto::Uint64Array {
                            values: vec![n % 128],
                        }))
                    }
                }
                _ => Err(PublishError::MetadataError),
            },
            None => Ok(proto::datapoint::Value::Uint64Array(proto::Uint64Array {
                values: vec![n % 128],
            })),
        },
        Ok(proto::DataType::FloatArray) => match &metadata.value_restriction {
            Some(value_restriction) => match &value_restriction.r#type {
                Some(proto::value_restriction::Type::FloatingPoint(values)) => {
                    if values.max.is_some() || values.min.is_some() {
                        let min = values.min.unwrap_or(f32::MIN.into()) as f32;
                        let max = values.max.unwrap_or(f32::MAX.into()) as f32;
                        let mut value = min + n as f32;
                        if value > max {
                            value %= max;
                            if value < min {
                                value = min;
                            }
                        }
                        Ok(proto::datapoint::Value::FloatArray(proto::FloatArray {
                            values: vec![value],
                        }))
                    } else if !values.allowed_values.is_empty() {
                        let index = n % values.allowed_values.len() as u64;
                        let value = values.allowed_values[index as usize] as f32;
                        Ok(proto::datapoint::Value::FloatArray(proto::FloatArray {
                            values: vec![value],
                        }))
                    } else {
                        Ok(proto::datapoint::Value::FloatArray(proto::FloatArray {
                            values: vec![n as f32],
                        }))
                    }
                }
                _ => Err(PublishError::MetadataError),
            },
            None => Ok(proto::datapoint::Value::FloatArray(proto::FloatArray {
                values: vec![n as f32],
            })),
        },
        Ok(proto::DataType::DoubleArray) => match &metadata.value_restriction {
            Some(value_restriction) => match &value_restriction.r#type {
                Some(proto::value_restriction::Type::FloatingPoint(values)) => {
                    if values.max.is_some() || values.min.is_some() {
                        let min = values.min.unwrap_or(f32::MIN.into()) as f32;
                        let max = values.max.unwrap_or(f32::MAX.into()) as f32;
                        let mut value = min + n as f32;
                        if value > max {
                            value %= max;
                            if value < min {
                                value = min;
                            }
                        }
                        Ok(proto::datapoint::Value::DoubleArray(proto::DoubleArray {
                            values: vec![value as f64],
                        }))
                    } else if !values.allowed_values.is_empty() {
                        let index = n % values.allowed_values.len() as u64;
                        let value = values.allowed_values[index as usize] as f32;
                        Ok(proto::datapoint::Value::DoubleArray(proto::DoubleArray {
                            values: vec![value as f64],
                        }))
                    } else {
                        Ok(proto::datapoint::Value::DoubleArray(proto::DoubleArray {
                            values: vec![n as f64],
                        }))
                    }
                }
                _ => Err(PublishError::MetadataError),
            },
            None => Ok(proto::datapoint::Value::DoubleArray(proto::DoubleArray {
                values: vec![n as f64],
            })),
        },
        Err(_) => Err(PublishError::MetadataError),
    }
}
