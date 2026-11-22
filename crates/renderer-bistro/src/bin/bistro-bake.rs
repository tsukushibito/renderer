fn main() -> Result<(), Box<dyn std::error::Error>> {
    renderer_bistro::run_bake(std::env::args().skip(1))
}
