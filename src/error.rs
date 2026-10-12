use serde::Serialize;
use serde_json::Value;
use std::fmt;

#[derive(Clone, Debug, Serialize)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

#[derive(Debug)]
pub struct AppError {
    pub code: &'static str,
    pub message: String,
    pub exit_code: i32,
    pub result: Value,
    pub details: Option<Value>,
}

impl AppError {
    pub fn operational(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            exit_code: 1,
            result: Value::Null,
            details: None,
        }
    }

    pub fn usage(message: impl Into<String>) -> Self {
        Self::usage_code("E_USAGE", message)
    }

    pub fn usage_code(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            exit_code: 2,
            result: Value::Null,
            details: None,
        }
    }

    pub fn locked(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            exit_code: 4,
            result: Value::Null,
            details: None,
        }
    }

    pub fn partial(result: Value) -> Self {
        Self {
            code: "E_PARTIAL",
            message: "operation completed with one or more failures".into(),
            exit_code: 3,
            result,
            details: None,
        }
    }

    pub fn io(context: impl fmt::Display, error: impl fmt::Display) -> Self {
        Self::operational("E_IO", format!("{context}: {error}"))
    }

    pub fn with_result(mut self, result: Value) -> Self {
        self.result = result;
        self
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for AppError {}

pub type AppResult<T> = Result<T, AppError>;

#[derive(Debug, Serialize)]
pub struct Envelope {
    pub format_version: u32,
    pub command: String,
    pub ok: bool,
    pub exit_code: i32,
    pub error: Option<ErrorBody>,
    pub result: Value,
}

impl Envelope {
    pub fn success(command: impl Into<String>, result: Value) -> Self {
        Self {
            format_version: 1,
            command: command.into(),
            ok: true,
            exit_code: 0,
            error: None,
            result,
        }
    }

    pub fn failure(command: impl Into<String>, error: &AppError) -> Self {
        Self {
            format_version: 1,
            command: command.into(),
            ok: false,
            exit_code: error.exit_code,
            error: Some(ErrorBody {
                code: error.code.to_string(),
                message: error.message.clone(),
                details: error.details.clone(),
            }),
            result: error.result.clone(),
        }
    }
}
