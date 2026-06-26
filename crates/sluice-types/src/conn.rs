//! The connection value type the UI renders.
//!
//! A plain Rust struct — Sluice's engine speaks its own gRPC contract (`sluice-proto` /
//! `ConnEvent`); this is just the shape the feed and verdict logic work with. Kept under a `pb`
//! module so the call sites read `pb::Connection`.

pub mod pb {
    /// One process-tree ancestor entry (comm + pid), used by the grouping heuristic. The engine
    /// doesn't currently provide a tree, so this is usually empty.
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct ProcessEntry {
        pub key: String,
        pub value: u32,
    }

    /// A connection as the UI models it: the destination, the owning process, and addressing.
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct Connection {
        pub protocol: String,
        pub src_ip: String,
        pub src_port: u32,
        pub dst_ip: String,
        pub dst_host: String,
        pub dst_port: u32,
        pub user_id: u32,
        pub process_id: u32,
        pub process_path: String,
        pub process_args: Vec<String>,
        pub process_tree: Vec<ProcessEntry>,
    }
}
