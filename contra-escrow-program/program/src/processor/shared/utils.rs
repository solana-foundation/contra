#[macro_export]
macro_rules! require_len {
    ($data:expr, $len:expr) => {
        if $data.len() < $len {
            return Err(ProgramError::InvalidInstructionData);
        }
    };
}

#[macro_export]
macro_rules! require {
    ($condition:expr, $error:expr) => {
        if !$condition {
            return Err($error.into());
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
macro_rules! validate_event_accounts {
    ($event_authority_info:expr, $program_info:expr) => {
        use $crate::constants::event_authority_pda;
        if $event_authority_info.address() != &event_authority_pda::ID {
            return Err($crate::error::ContraEscrowProgramError::InvalidEventAuthority.into());
        }
        $crate::processor::shared::account_check::verify_current_program($program_info)?;
    };
}
