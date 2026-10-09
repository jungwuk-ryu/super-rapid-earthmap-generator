pub(crate) fn known_command_usage_error(command: &str, arg_count: usize) -> Option<String> {
    let usage = match command {
        "generate" if arg_count < 10 => {
            "Usage: generate <heightmap> <worldDir> <scale> <startRegionX> <startRegionZ> <cols> <rows> <mca|linear> <threads|auto> [surface|carvers] [surfaceRaster=auto|path] [options...]"
        }
        "generate-vanilla-delegated-region" if arg_count < 6 => {
            "Usage: generate-vanilla-delegated-region [heightmap] <worldDir> <scale> <regionX> <regionZ> <mca|linear> [surface|carvers] [surfaceRaster=auto|path] [options...]"
        }
        "generate-vanilla-delegated-regions-parallel" if arg_count < 10 => {
            "Usage: generate-vanilla-delegated-regions-parallel <heightmap> <worldDir> <scale> <startRegionX> <startRegionZ> <cols> <rows> <mca|linear> <threads|auto> [surface|carvers] [surfaceRaster=auto|path] [options...]"
        }
        _ => return None,
    };
    Some(usage.to_string())
}
