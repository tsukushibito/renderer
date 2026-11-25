fn main() -> Result<(), Box<dyn std::error::Error>> {
    renderer_scenes::run_bake(std::env::args().skip(1))
}
