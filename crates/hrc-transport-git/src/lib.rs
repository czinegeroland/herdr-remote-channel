//! Built-in Git transport adapter for Herdr Remote Channel.
//!
//! Publishes and fetches immutable objects on the machine-managed `hrc`
//! branch by invoking the system Git executable, so existing credential
//! helpers stay authoritative. Publication is an optimistic fast-forward
//! compare-and-swap; it never force-pushes.
//!
//! See PRD sections 16 and 17. Implementation lands in milestone M2.
