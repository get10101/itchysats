#[macro_export]
macro_rules! impl_sqlx_type_display_from_str {
    ($ty:ty) => {
        impl sqlx::Type<sqlx::Postgres> for $ty {
            fn type_info() -> sqlx::postgres::PgTypeInfo {
                String::type_info()
            }
        }

        impl sqlx::Encode<'_, sqlx::Postgres> for $ty {
            fn encode_by_ref(
                &self,
                args: &mut sqlx::postgres::PgArgumentBuffer,
            ) -> sqlx::encode::IsNull {
                self.to_string().encode_by_ref(args)
            }
        }

        impl<'r> sqlx::Decode<'r, sqlx::Postgres> for $ty {
            fn decode(
                value: sqlx::postgres::PgValueRef<'r>,
            ) -> Result<Self, sqlx::error::BoxDynError> {
                let string = String::decode(value)?;
                let value = string.parse()?;

                Ok(value)
            }
        }
    };
}
