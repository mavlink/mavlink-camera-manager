use std::time::Duration;

/// List-after-POST / TestEnv setup.
pub const SETUP: Duration = Duration::from_secs(15);

/// RTSP OPTIONS until the factory answers; H265 lazy encoders need this budget.
pub const FACTORY_READY: Duration = Duration::from_secs(60);

/// First encoded frames on a non-lazy data-flow stream.
pub const FIRST_FRAME: Duration = Duration::from_secs(30);

/// TCP connect inside `RtspClient::new`.
pub const TCP_CONNECT: Duration = Duration::from_secs(15);
