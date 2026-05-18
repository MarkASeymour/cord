pub mod blob;
pub mod contact;
pub mod store;

pub use blob::{BlobError, ContactBlob, BLOB_VERSION, MAX_DISPLAY_NAME, SCHEME_PREFIX};
pub use contact::{Contact, ContactStatus};
pub use store::{load, save, StoreError};
