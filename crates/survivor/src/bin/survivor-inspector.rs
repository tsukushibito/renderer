use std::error::Error;

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        let [op, catalog, model, clip, sample, rate] = args.as_slice() else {
            return Err("expected clip CATALOG MODEL CLIP SAMPLE RATE".into());
        };
        if op != "clip" {
            return Err(format!("unknown command {op}").into());
        }
        let mesh = sashiko::authoring::clip_pose(
            catalog.as_ref(),
            model,
            clip,
            sample.parse()?,
            rate.parse()?,
        )?;
        return Ok(serde_json::to_writer(std::io::stdout().lock(), &mesh)?);
    }
    sashiko::authoring::serve_once(&survivor::authoring::registry(), |source| {
        survivor::Arena::from_json(source).map(|_| ())
    })
}
