use std::error::Error;

fn main() -> Result<(), Box<dyn Error>> {
    sashiko::authoring::serve_once(&survivor::authoring::registry(), |source| {
        survivor::Arena::from_json(source).map(|_| ())
    })
}
