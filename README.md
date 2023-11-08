Plots autospectra in the terminal!


The plotter can grab live spectrum from the correlator, or load the `.npy` output files from RFIMonitorTools.

```bash
Usage: spectrum-tui <COMMAND>

Commands:
  file  Plot spectra from an RFIMonitorTool output npy file
  live  Watch live autospectra from the correlator
  help  Print this message or the help of the given subcommand(s)

Options:
  -h, --help     Print help
  -V, --version  Print version
```

When watching live autos the plotter can accept either a single antenna or space-separated list of antenna names to watch.
```bash
Usage: spectrum-tui live [OPTIONS] [ANTENNA]...

Arguments:
  [ANTENNA]...
          The Antenna Name(s) to grab autos

          This should be a string like LWA-250.

          This antenna name is matched against the configuration name exactly.

          This can also be a space separated list of antennas: LWA-124 LWA-250 ...etc

Options:
  -d, --delay <DELAY>
          The interval in seconds at which to poll for new autos

          [default: 30]

  -h, --help
          Print help (see a summary with '-h')```