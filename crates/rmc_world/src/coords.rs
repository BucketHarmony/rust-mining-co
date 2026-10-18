//! Cell, chunk, local and world-space coordinate conversions (SAD §5).

#[cfg(test)]
mod tests {
    #[test]
    fn smoke_compiles() {
        // The workspace builds and the core crate links glam at Bevy's minor.
        assert_eq!(glam::IVec3::ZERO + glam::IVec3::ONE, glam::IVec3::ONE);
    }
}
