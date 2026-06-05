use earthmap_geo::{GeoTiffFloat32Reader, GeoTiffSingleBandReader};
use std::env;
use std::path::Path;

fn main() {
    let args = env::args().collect::<Vec<_>>();
    if args.len() != 5 {
        eprintln!("usage: sample_single_band <u8|float32> <path> <longitude> <latitude>");
        std::process::exit(2);
    }
    let kind = &args[1];
    let path = Path::new(&args[2]);
    let longitude = args[3].parse::<f64>().expect("longitude");
    let latitude = args[4].parse::<f64>().expect("latitude");
    if kind == "float32" {
        let reader = GeoTiffFloat32Reader::open(path).expect("open float32");
        let x = reader.pixel_x(longitude);
        let y = reader.pixel_y(latitude);
        println!("x={x}");
        println!("y={y}");
        println!("metadata={:?}", reader.metadata());
        println!("sampleAtPixel={:?}", reader.sample_at_pixel(x, y));
        println!(
            "sampleNearest={:?}",
            reader.sample_nearest(longitude, latitude)
        );
    } else {
        let reader = GeoTiffSingleBandReader::open(path).expect("open single-band");
        let x = reader.pixel_x(longitude);
        let y = reader.pixel_y(latitude);
        println!("x={x}");
        println!("y={y}");
        println!("metadata={:?}", reader.metadata());
        println!("sampleAtPixel={:?}", reader.sample_at_pixel(x, y));
        println!(
            "sampleNearest={:?}",
            reader.sample_nearest(longitude, latitude)
        );
    }
}
