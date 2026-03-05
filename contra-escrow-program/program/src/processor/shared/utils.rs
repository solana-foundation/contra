#[macro_export]
macro_rules! require_len {
    ($data:expr, $len:expr) => {
        if $data.len() < $len {
            return Err(ProgramError::InvalidInstructionData);
        }
    };
}

#[macro_export]
macro_rules! validate_discriminator {
    ($data:expr, $discriminator:expr) => {
        if $data.is_empty() || $data[0] != $discriminator {
            return Err(ProgramError::InvalidAccountData);
        }
    };
}

#[macro_export]
macro_rules! validate_event_authority {
    ($event_authority_info:expr) => {
        if $event_authority_info.address()
            != &$crate::constants::event_authority_pda::ID
        {
            return Err($crate::error::ContraEscrowProgramError::InvalidEventAuthority.into());
        }
    };
}
