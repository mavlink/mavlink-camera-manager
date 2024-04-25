#[derive(Default, Clone)]
pub enum RTSPScheme {
    #[default]
    Rtsp,
    Rtspu,
    Rtspt,
    Rtsph,
    Rtsps,
    Rtspsu,
    Rtspst,
    Rtspsh,
}

impl RTSPScheme {
    pub const VALUES: [Self; 8] = [
        Self::Rtsp,
        Self::Rtspu,
        Self::Rtspt,
        Self::Rtsph,
        Self::Rtsps,
        Self::Rtspsu,
        Self::Rtspst,
        Self::Rtspsh,
    ];
}

impl std::fmt::Debug for RTSPScheme {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rtsp => write!(f, "rtsp"),
            Self::Rtspu => write!(f, "rtspu"),
            Self::Rtspt => write!(f, "rtspt"),
            Self::Rtsph => write!(f, "rtsph"),
            Self::Rtsps => write!(f, "rtsps"),
            Self::Rtspsu => write!(f, "rtspsu"),
            Self::Rtspst => write!(f, "rtspst"),
            Self::Rtspsh => write!(f, "rtspsh"),
        }
    }
}

impl TryFrom<&str> for RTSPScheme {
    type Error = String;

    fn try_from(value: &str) -> std::prelude::v1::Result<Self, Self::Error> {
        match value.to_lowercase().as_str() {
            "rtsp" => Ok(Self::Rtsp),
            "rtspu" => Ok(Self::Rtspu),
            "rtspt" => Ok(Self::Rtspt),
            "rtsph" => Ok(Self::Rtsph),
            "rtsps" => Ok(Self::Rtsps),
            "rtspsu" => Ok(Self::Rtspsu),
            "rtspst" => Ok(Self::Rtspst),
            "rtspsh" => Ok(Self::Rtspsh),
            _ => Err("Uknown RTSP scheme variant".to_string()),
        }
    }
}
