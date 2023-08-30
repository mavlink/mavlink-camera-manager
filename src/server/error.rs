use actix_web::{http::StatusCode, ResponseError};

use paperclip::actix::api_v2_errors;
use validator::ValidationErrors;

pub type Result<T> = actix_web::Result<T, Error>;

#[allow(dead_code)]
#[api_v2_errors(
    code = 400,
    description = "Bad Request: The client's request contains invalid or malformed data.",
    code = 404,
    description = "Not Found: The requested path or entity does not exist.",
    code = 500,
    description = "Internal Server Error: An unexpected server error has occurred.",
    code = 503,
    description = "Service Unavailable: ."
)]
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Bad Request: {0}")]
    BadRequest(String),

    #[error("Not Found: {0}")]
    NotFound(String),

    #[error("Internal Server Error: {0}")]
    Internal(String),

    #[error("Service Unavailable: {0}")]
    Unavailable(String),
}

impl ResponseError for Error {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
        }
    }
}

impl From<ValidationErrors> for Error {
    fn from(error: ValidationErrors) -> Self {
        Self::BadRequest(error.to_string())
    }
}

impl From<actix_web_validator::Error> for Error {
    fn from(error: actix_web_validator::Error) -> Self {
        Self::BadRequest(error.to_string())
    }
}
