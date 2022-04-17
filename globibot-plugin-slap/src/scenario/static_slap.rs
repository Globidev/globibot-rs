use super::SlapScenario;

use common::anyhow;

pub fn load_scenario() -> anyhow::Result<SlapScenario> {
    let path = super::img_path("slap-hd.png");
    let image = common::image::open(path)?.into_rgba8();

    let scenario = SlapScenario {
        dim: (2560, 1707),
        avatar_dim: (300, 300),
        slapper_positions: vec![(1631, 207)],
        slapped_positions: vec![(751, 266)],
        frames: vec![image],
    };

    Ok(scenario)
}
