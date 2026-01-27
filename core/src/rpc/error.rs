use jsonrpsee::types::ErrorObjectOwned;

pub fn custom_error(code: i32, message: impl ToString) -> ErrorObjectOwned {
    ErrorObjectOwned::owned(code, message.to_string(), None::<()>)
}

pub fn read_not_enabled() -> ErrorObjectOwned {
    custom_error(-32002, "Read operations not enabled")
}

pub fn write_not_enabled() -> ErrorObjectOwned {
    custom_error(-32001, "Write operations not enabled")
}
