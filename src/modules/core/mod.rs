// ├── core/                       # Core functionality (platform-independent)
// │   ├── mod.rs                  # Core library entry point
// │   ├── traversal.rs            # Directory traversal logic (platform-agnostic)
// │   ├── entry_mapper.rs         # Mapping DirEntry to custom DirEntry struct
// │   ├── metadata.rs             # File metadata extraction
// │   ├── filters.rs              # Path/file filters, exclusions, etc.
// │   └── polars_adapter.rs       # Adapter for Polars integration, transforms data into a dataframe

mod traversal;
mod entry_manager;
mod metadata;
mod filters;
mod polars_adapter;