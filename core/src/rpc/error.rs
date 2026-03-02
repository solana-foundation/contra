use jsonrpsee::types::ErrorObjectOwned;

pub use jsonrpsee::types::error::{
    INTERNAL_ERROR_CODE, INVALID_PARAMS_CODE, INVALID_REQUEST_CODE, PARSE_ERROR_CODE,
};

/// Generic JSON-RPC server error (base of the -32000..-32099 reserved range).
pub const JSON_RPC_SERVER_ERROR: i32 = -32000;

pub fn custom_error(code: i32, message: impl ToString) -> ErrorObjectOwned {
    ErrorObjectOwned::owned(code, message.to_string(), None::<()>)
}

pub fn read_not_enabled() -> ErrorObjectOwned {
    custom_error(-32002, "Read operations not enabled")
}

pub fn write_not_enabled() -> ErrorObjectOwned {
    custom_error(-32001, "Write operations not enabled")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_custom_error() {
        let err = custom_error(-32000, "test error");
        assert_eq!(err.code(), -32000);
        assert_eq!(err.message(), "test error");
    }

    #[test]
    fn test_read_not_enabled_code() {
        let err = read_not_enabled();
        assert_eq!(err.code(), -32002);
        assert!(err.message().contains("Read"));
    }

    #[test]
    fn test_write_not_enabled_code() {
        let err = write_not_enabled();
        assert_eq!(err.code(), -32001);
        assert!(err.message().contains("Write"));
    }

    #[test]
    fn test_json_rpc_server_error_constant() {
        assert_eq!(JSON_RPC_SERVER_ERROR, -32000);
    }
}
