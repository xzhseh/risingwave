// Copyright 2023 RisingWave Labs
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::borrow::Cow;
use std::error::Error;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use thiserror_ext::AsReport;
use tonic::metadata::{MetadataMap, MetadataValue};

/// The key of the metadata field that contains the serialized error.
const ERROR_KEY: &str = "risingwave-error-bin";

/// The service name that the error is from. Used to provide better error message.
type ServiceName = Cow<'static, str>;

/// The error produced by the gRPC server and sent to the client on the wire.
#[derive(Debug, Serialize, Deserialize)]
struct ServerError {
    error: serde_error::Error,
    service_name: Option<ServiceName>,
}

impl std::fmt::Display for ServerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}

impl std::error::Error for ServerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.error.source()
    }
}

fn to_status<T>(error: &T, code: tonic::Code, service_name: Option<ServiceName>) -> tonic::Status
where
    T: ?Sized + std::error::Error,
{
    // Embed the whole error (`self`) and its source chain into the details field.
    // At the same time, set the message field to the error message of `self` (without source chain).
    // The redundancy of the current error's message is intentional in case the client ignores the `details` field.
    let source = ServerError {
        error: serde_error::Error::new(error),
        service_name,
    };
    let serialized = bincode::serialize(&source).unwrap();

    let mut metadata = MetadataMap::new();
    metadata.insert_bin(ERROR_KEY, MetadataValue::from_bytes(&serialized));

    let mut status = tonic::Status::with_metadata(code, error.to_report_string(), metadata);
    // Set the source of `tonic::Status`, though it's not likely to be used.
    // This is only available before serializing to the wire. That's why we need to manually embed it
    // into the `details` field.
    status.set_source(Arc::new(source));
    status
}

// TODO(error-handling): disallow constructing `tonic::Status` directly with `new` by clippy.
#[easy_ext::ext(ToTonicStatus)]
impl<T> T
where
    T: ?Sized + std::error::Error,
{
    /// Convert the error to [`tonic::Status`] with the given [`tonic::Code`] and service name.
    ///
    /// The source chain is preserved by pairing with [`TonicStatusWrapper`].
    pub fn to_status(
        &self,
        code: tonic::Code,
        service_name: impl Into<ServiceName>,
    ) -> tonic::Status {
        to_status(self, code, Some(service_name.into()))
    }

    /// Convert the error to [`tonic::Status`] with the given [`tonic::Code`] without specifying
    /// the service name. Prefer [`to_status`] if possible.
    ///
    /// The source chain is preserved by pairing with [`TonicStatusWrapper`].
    pub fn to_status_unnamed(&self, code: tonic::Code) -> tonic::Status {
        to_status(self, code, None)
    }
}

/// A wrapper of [`tonic::Status`] that provides better error message and extracts
/// the source chain from the `details` field.
#[derive(Debug)]
pub struct TonicStatusWrapper(tonic::Status);

impl TonicStatusWrapper {
    /// Create a new [`TonicStatusWrapper`] from the given [`tonic::Status`] and extract
    /// the source chain from its `details` field.
    pub fn new(mut status: tonic::Status) -> Self {
        if status.source().is_none() {
            if let Some(value) = status.metadata().get_bin(ERROR_KEY) {
                if let Some(e) = value.to_bytes().ok().and_then(|serialized| {
                    bincode::deserialize::<ServerError>(serialized.as_ref()).ok()
                }) {
                    status.set_source(Arc::new(e));
                } else {
                    tracing::warn!("failed to deserialize error from gRPC metadata");
                }
            }
        }
        Self(status)
    }

    /// Returns the reference to the inner [`tonic::Status`].
    pub fn inner(&self) -> &tonic::Status {
        &self.0
    }

    /// Consumes `self` and returns the inner [`tonic::Status`].
    pub fn into_inner(self) -> tonic::Status {
        self.0
    }
}

impl From<tonic::Status> for TonicStatusWrapper {
    fn from(status: tonic::Status) -> Self {
        Self::new(status)
    }
}

impl std::fmt::Display for TonicStatusWrapper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "gRPC request")?;
        if let Some(service_name) = self
            .source()
            .and_then(|s| s.downcast_ref::<ServerError>())
            .and_then(|s| s.service_name.as_ref())
        {
            write!(f, " to {} service", service_name)?;
        }
        write!(f, " failed: {}: ", self.0.code())?;
        #[expect(rw::format_error)] // intentionally format the source itself
        if let Some(source) = self.source() {
            // Prefer the source chain from the `details` field.
            write!(f, "{}", source)
        } else {
            write!(f, "{}", self.0.message())
        }
    }
}

impl std::error::Error for TonicStatusWrapper {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        // Delegate to `self.0` as if we're transparent.
        self.0.source()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_source_chain_preserved() {
        #[derive(thiserror::Error, Debug)]
        #[error("{message}")]
        struct MyError {
            message: &'static str,
            source: Option<Box<MyError>>,
        }

        let original = MyError {
            message: "outer",
            source: Some(Box::new(MyError {
                message: "inner",
                source: None,
            })),
        };

        let server_status = original.to_status(tonic::Code::Internal, "test");
        let body = server_status.to_http();
        let client_status = tonic::Status::from_header_map(body.headers()).unwrap();

        let wrapper = TonicStatusWrapper::new(client_status);
        assert_eq!(
            wrapper.to_string(),
            "gRPC request to test service failed: Internal error: outer"
        );

        let source = wrapper.source().unwrap();
        assert!(source.is::<ServerError>());
        assert_eq!(source.to_string(), "outer");
        assert_eq!(source.source().unwrap().to_string(), "inner");
    }
}
