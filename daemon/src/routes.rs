use rocket::http::ContentType;
use rocket::http::Status;
use rust_embed::EmbeddedFile;
use std::borrow::Cow;
use std::path::PathBuf;

pub trait EmbeddedFileExt {
    fn into_response(self, file: PathBuf) -> Result<(ContentType, Cow<'static, [u8]>), Status>;
}

impl EmbeddedFileExt for Option<EmbeddedFile> {
    fn into_response(self, file: PathBuf) -> Result<(ContentType, Cow<'static, [u8]>), Status> {
        let embedded_file = self.ok_or(Status::NotFound)?;
        let ext = file
            .as_path()
            .extension()
            .ok_or(Status::BadRequest)?
            .to_str()
            .ok_or(Status::InternalServerError)?;
        let content_type = ContentType::from_extension(ext).ok_or(Status::InternalServerError)?;

        Ok((content_type, embedded_file.data))
    }
}
