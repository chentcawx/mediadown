use std::io;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum AppError {
    #[error("IO 错误: {0}")]
    Io(#[from] io::Error),

    #[error("HTTP 请求错误: {0}")]
    Http(#[from] reqwest::Error),

    #[error("JSON 解析错误: {0}")]
    Json(#[from] serde_json::Error),

    #[error("URL 解析错误: {0}")]
    Url(#[from] url::ParseError),

    #[error("UTF-8 解码错误: {0}")]
    Utf8(#[from] std::str::Utf8Error),

    #[error("整数解析错误: {0}")]
    ParseInt(#[from] std::num::ParseIntError),

    #[error("MP4 解析错误: {0}")]
    Mp4Parse(String),

    #[error("配置错误: {0}")]
    Config(String),

    #[error("未找到: {0}")]
    NotFound(String),

    #[error("操作已中止")]
    Aborted,

    #[error("参数无效: {0}")]
    InvalidArg(String),

    #[error("权限不足: {0}")]
    Forbidden(String),

    #[error("请求体过大: {0}")]
    PayloadTooLarge(String),

    #[error("服务器内部错误: {0}")]
    Internal(String),

    #[error("混流失败: {0}")]
    MuxFailed(String),

    #[error("下载失败: {0}")]
    DownloadFailed(String),

    #[error("文件已存在: {0}")]
    FileExists(String),

    #[error("目录创建失败: {0}")]
    DirCreateFailed(String),
}

pub type AppResult<T> = Result<T, AppError>;

impl From<AppError> for String {
    fn from(e: AppError) -> Self {
        e.to_string()
    }
}

impl From<String> for AppError {
    fn from(s: String) -> Self {
        AppError::Internal(s)
    }
}

impl From<&str> for AppError {
    fn from(s: &str) -> Self {
        AppError::Internal(s.to_string())
    }
}

impl serde::Serialize for AppError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

#[macro_export]
macro_rules! bail {
    ($($arg:tt)*) => {
        return Err($crate::error::AppError::Internal(format!($($arg)*)).into())
    };
}

#[macro_export]
macro_rules! ensure {
    ($cond:expr, $($arg:tt)*) => {
        if !$cond {
            bail!($($arg)*);
        }
    };
}