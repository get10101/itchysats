use crate::models::User;
use crate::Connection;
use anyhow::Result;

// we only want to have max 1 user, hence, we hardcode its ID to 1
const USER_ID: u8 = 1;

impl Connection {
    pub async fn load_user(self) -> Result<Option<User>> {
        let mut conn = self.inner.acquire().await?;
        let row = sqlx::query!(
            r#"
            SELECT * from login_details where id = $1
            "#,
            USER_ID
        )
        .fetch_optional(&mut *conn)
        .await?;

        match row {
            Some(row) => Ok(Some(User {
                id: row.id as u32,
                password: row.PASSWORD,
                first_login: row.first_login,
            })),
            None => Ok(None),
        }
    }

    pub async fn update_password(self, password: String) -> Result<()> {
        let mut conn = self.inner.acquire().await?;
        sqlx::query!(
            r#"
            UPDATE login_details
            SET password = $1, first_login = false
            WHERE id = $2
            "#,
            password,
            USER_ID
        )
        .execute(&mut *conn)
        .await?;
        Ok(())
    }
}
