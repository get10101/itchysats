use crate::auth::rand_string;
use crate::error::Error;
use crate::forms::Login;
use crate::session::Session;
use crate::session::SessionManager;
use crate::user::verify_password;
use crate::user::User;
use crate::Database;
use anyhow::Context;
use anyhow::Result;

pub struct Users {
    db: Box<dyn Database>,
    sessions: Box<dyn SessionManager>,
}

impl Users {
    pub fn new(db: Box<dyn Database>) -> Users {
        Self {
            db,
            sessions: Box::new(chashmap::CHashMap::new()),
        }
    }

    /// Check if a user is authenticated with the provided session
    pub(crate) fn is_auth(&self, session: &Session) -> Result<bool> {
        let session_id = session.id;
        let option = self.sessions.get(session_id)?;
        if let Some(auth_key) = option {
            Ok(session.auth_key.clone() == auth_key)
        } else {
            Ok(false)
        }
    }

    pub(crate) async fn login(&self, form: &Login) -> Result<User, Error> {
        let user = self
            .db
            .load_user()
            .await
            .map_err(Error::Other)?
            .context(Error::UserNotFound)?;

        let user_pwd = &user.password;
        if verify_password(user_pwd, form.password.as_str())? {
            let auth_key = self.set_auth_key(user.id);
            Ok(User {
                id: user.id,
                password: user.password,
                auth_key,
                first_login: user.first_login,
            })
        } else {
            Err(Error::Unauthorized)
        }
    }

    pub(crate) fn logout(&self, session: &Session) -> Result<()> {
        if self.is_auth(session)? {
            self.sessions.remove(session.id);
        }
        Ok(())
    }

    fn set_auth_key(&self, user_id: u32) -> String {
        let key = rand_string(15);
        self.sessions
            .insert_for(user_id, key.clone(), time::Duration::days(7));
        key
    }

    pub async fn get_by_id(&self) -> Result<Option<User>> {
        let maybe_user = self.db.load_user().await?;
        Ok(maybe_user)
    }
    pub async fn update_user(&self, user: User) -> Result<()> {
        self.db.update_password(user.password).await?;
        Ok(())
    }
}
