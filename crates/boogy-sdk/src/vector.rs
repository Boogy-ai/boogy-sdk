//! SDK types for vector search (used by component authors).
//!
//! These types are the SDK-facing surface. The WIT-generated types live
//! under `$bindings::boogy::platform::vector::*` and are only available
//! in crates that invoke `wit_bindgen::generate!`. The `wit_glue!` macro
//! bridges between these SDK types and the WIT types.

/// Distance metric for vector collections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DistanceMetric {
    Cosine,
    Euclidean,
    DotProduct,
}

/// A single vector search result.
#[derive(Debug, Clone)]
pub struct VectorResult {
    pub rowid: u64,
    pub distance: f32,
}

/// Options for creating a vector collection with custom HNSW parameters.
#[derive(Debug, Clone)]
pub struct VectorCollectionOptions {
    pub dimensions: u32,
    pub metric: DistanceMetric,
    pub m: Option<u32>,
    pub ef_construction: Option<u32>,
}
