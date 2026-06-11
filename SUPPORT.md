# Support

Use GitHub issues for reproducible bugs, documentation gaps, and feature requests.

Before filing a bug:

```powershell
.\scripts\build.ps1
.\scripts\test.ps1 -Filter <relevant-pattern>
```

For generation problems, include:

- command line or GUI settings
- output format: `mca` or `linear`
- scale and region grid
- relevant progress lines or JSON events
- whether the run used external rasters via `surfaceRaster=auto` or an explicit path

Do not attach proprietary rasters, Minecraft server jars, generated full worlds, secrets, or private logs. Summaries and
small redacted snippets are preferred.
