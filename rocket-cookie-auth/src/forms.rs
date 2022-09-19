use crate::error::Error;
use anyhow::Result;
use serde::Deserialize;

/// The `Login` form is used along with the [`Auth`] guard to authenticate users.
#[derive(FromForm, Deserialize, Clone, Hash, PartialEq, Eq)]
pub struct Login {
    pub(crate) password: String,
}

/// The `ChangePassword` form is used along with the [`User`] guard to change a user's password
#[derive(FromForm, Deserialize, Clone, Hash, PartialEq, Eq)]
pub struct ChangePassword {
    pub password: String,
}

impl ChangePassword {
    pub fn is_secure(&self) -> Result<()> {
        let password = self.password.as_str();
        is_long(password)?;
        has_uppercase(password)?;
        has_lowercase(password)?;
        has_number(password)?;
        Ok(())
    }
}

fn is_long(password: &str) -> Result<(), Error> {
    if password.len() < 8 {
        return Err(Error::PasswordValidation(
            "The password must be at least 8 characters long".to_string(),
        ));
    }
    Ok(())
}

fn has_uppercase(password: &str) -> Result<(), Error> {
    for c in password.chars() {
        if c.is_uppercase() {
            return Ok(());
        }
    }
    Err(Error::PasswordValidation(
        "The password must include at least one uppercase character.".to_string(),
    ))
}

fn has_lowercase(password: &str) -> Result<(), Error> {
    for c in password.chars() {
        if c.is_lowercase() {
            return Ok(());
        }
    }
    Err(Error::PasswordValidation(
        "The password must include at least one uppercase character.".to_string(),
    ))
}

fn has_number(password: &str) -> Result<(), Error> {
    for c in password.chars() {
        if c.is_numeric() {
            return Ok(());
        }
    }
    Err(Error::PasswordValidation(
        "The password has to contain at least one digit.".to_string(),
    ))
}
