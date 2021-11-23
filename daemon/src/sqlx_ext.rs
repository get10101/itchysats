#[macro_export]
macro_rules! impl_sqlx_type_display_from_str {
    ($ty:ty) => {
        impl sqlx::Type<sqlx::Sqlite> for $ty {
            fn type_info() -> sqlx::sqlite::SqliteTypeInfo {
                String::type_info()
            }
        }

        impl<'q> sqlx::Encode<'q, sqlx::Sqlite> for $ty {
            fn encode_by_ref(
                &self,
                args: &mut Vec<sqlx::sqlite::SqliteArgumentValue<'q>>,
            ) -> sqlx::encode::IsNull {
                self.to_string().encode_by_ref(args)
            }
        }

        impl<'r> sqlx::Decode<'r, sqlx::Sqlite> for $ty {
            fn decode(
                value: sqlx::sqlite::SqliteValueRef<'r>,
            ) -> Result<Self, sqlx::error::BoxDynError> {
                let string = String::decode(value)?;
                let value = string.parse()?;

                Ok(value)
            }
        }
    };
}
