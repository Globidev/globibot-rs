use super::SlapScenario;

use common::{anyhow, imageops};

pub fn load_scenario() -> anyhow::Result<SlapScenario> {
    let path = super::img_path("slap-animated.gif");
    let frames = imageops::load_gif(path, DIMENSIONS)?;

    let scenario = SlapScenario {
        dim: DIMENSIONS,
        avatar_dim: (50, 50),
        slapper_positions: SMITH_POS.to_vec(),
        slapped_positions: ROCK_POS.to_vec(),
        frames,
    };

    Ok(scenario)
}

const DIMENSIONS: (u16, u16) = (480, 480);

const ROCK_POS: [(u32, u32); 47] = [
    (139, 138),
    (139, 138),
    (144, 136),
    (144, 137),
    (144, 139),
    (146, 137),
    (146, 139),
    (148, 136),
    (148, 137),
    (154, 138),
    (153, 138),
    (154, 138),
    (154, 140),
    (155, 138),
    (157, 139),
    (153, 140),
    (156, 138),
    (155, 138),
    (157, 136),
    (156, 136),
    (156, 136),
    (152, 136),
    (138, 138),
    (121, 150),
    (110, 164),
    (99, 166),
    (100, 166),
    (100, 162),
    (103, 160),
    (106, 158),
    (110, 152),
    (112, 150),
    (117, 158),
    (121, 148),
    (113, 147),
    (113, 147),
    (109, 149),
    (102, 151),
    (97, 152),
    (96, 153),
    (95, 153),
    (95, 153),
    (95, 150),
    (96, 147),
    (100, 147),
    (105, 146),
    (110, 146),
];

const SMITH_POS: [(u32, u32); 47] = [
    (278, 98),
    (276, 98),
    (274, 101),
    (271, 107),
    (270, 113),
    (268, 117),
    (264, 118),
    (261, 116),
    (256, 113),
    (251, 115),
    (248, 116),
    (241, 118),
    (233, 122),
    (228, 124),
    (221, 123),
    (219, 120),
    (216, 119),
    (216, 118),
    (213, 117),
    (212, 119),
    (211, 115),
    (212, 112),
    (214, 110),
    (217, 114),
    (219, 115),
    (215, 113),
    (212, 115),
    (210, 118),
    (212, 120),
    (216, 122),
    (220, 122),
    (224, 125),
    (232, 124),
    (233, 124),
    (239, 126),
    (243, 124),
    (244, 123),
    (248, 126),
    (250, 128),
    (254, 129),
    (260, 127),
    (269, 123),
    (272, 124),
    (274, 123),
    (278, 118),
    (281, 116),
    (286, 119),
];