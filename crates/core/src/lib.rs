//! Shared logic for Fizl ApiCraft: the catalog model, compressed-JSON storage,
//! Postman import/export, curl/PowerShell generation, auth schemes, SOAP,
//! gRPC, GraphQL and OData helpers, and the engine that runs requests and workflows. It does no
//! I/O itself (sending goes through [`http::Transport`]), so it runs both
//! natively and in wasm.

pub mod auth;
pub mod clock;
pub mod codegen;
pub mod curl_import;
pub mod engine;
pub mod extract;
pub mod folder;
pub mod form_login;
pub mod format;
pub mod git;
pub mod graphql;
pub mod grpc;
pub mod http;
pub mod ids;
pub mod mcp;
pub mod model;
pub mod odata;
pub mod openapi;
pub mod postman;
pub mod prepare;
pub mod soap;
pub mod sources;
pub mod store;
pub mod url;
pub mod vars;
pub mod workflow;
pub mod wsdl;

pub use model::*;
