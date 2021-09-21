use rocket::http::{ContentType, Status};
use rust_embed::EmbeddedFile;
use std::borrow::Cow;
use std::ffi::OsStr;
use std::path::PathBuf;

pub trait EmbeddedFileExt {
    fn into_response(self, file: PathBuf) -> Result<(ContentType, Cow<'static, [u8]>), Status>;
}

impl EmbeddedFileExt for Option<EmbeddedFile> {
    fn into_response(self, file: PathBuf) -> Result<(ContentType, Cow<'static, [u8]>), Status> {
        match self {
            None => Err(Status::NotFound),
            Some(embedded_file) => {
                let ext = file
                    .as_path()
                    .extension()
                    .and_then(OsStr::to_str)
                    .ok_or_else(|| Status::new(400))?;
                let content_type =
                    ContentType::from_extension(ext).ok_or_else(|| Status::new(400))?;
                Ok::<(ContentType, Cow<[u8]>), Status>((content_type, embedded_file.data))
            }
        }
    }
}
