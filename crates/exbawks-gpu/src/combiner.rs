//! The NV2A register combiners: how a fragment's color is computed.
//!
//! A title programs up to eight general stages and a final one. Each stage
//! reads four variables — `A`, `B`, `C`, `D` — from a small register file,
//! maps each through a signed or unsigned transform, and writes back up to
//! three results: `A·B`, `C·D`, and their sum (or a mux of them), any of
//! which can be scaled and biased. The final stage folds what is left into
//! the pixel.
//!
//! Everything here works in floating point over the register file, which is
//! how the hardware behaves: values may leave `0..=1` between stages and
//! only the final result is clamped.

/// General combiner stages the hardware provides.
pub const STAGES: usize = 8;

/// The register file one stage reads from and writes to.
#[derive(Debug, Clone, Copy, Default)]
pub struct Registers {
    /// Constant colors zero and one, per stage.
    pub constant0: [f32; 4],
    pub constant1: [f32; 4],
    /// The fog color and factor.
    pub fog: [f32; 4],
    /// The interpolated diffuse and specular colors.
    pub diffuse: [f32; 4],
    pub specular: [f32; 4],
    /// The four texture stages' sampled colors.
    pub textures: [[f32; 4]; 4],
    /// The two scratch registers stages pass results through.
    pub spare0: [f32; 4],
    pub spare1: [f32; 4],
}

/// One general stage, as its four control words describe it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stage {
    /// Which registers the color portion reads, and how they map.
    pub color_inputs: u32,
    /// The same for the alpha portion.
    pub alpha_inputs: u32,
    /// Where the color portion writes, and how it scales.
    pub color_outputs: u32,
    /// The same for the alpha portion.
    pub alpha_outputs: u32,
}

/// The whole combiner configuration a draw runs under.
#[derive(Debug, Clone, Copy)]
pub struct CombinerState {
    /// The general stages, in order.
    pub stages: [Stage; STAGES],
    /// How many of them run.
    pub active: usize,
    /// The final stage's two control words.
    pub final_first: u32,
    pub final_second: u32,
    /// The combiner control word, as the title wrote it.
    pub control: u32,
    /// Per-stage constant colors.
    pub factor0: [[f32; 4]; STAGES],
    pub factor1: [[f32; 4]; STAGES],
}

impl Default for CombinerState {
    fn default() -> Self {
        Self {
            stages: [Stage::default(); STAGES],
            active: 0,
            final_first: 0,
            final_second: 0,
            control: 0,
            factor0: [[0.0; 4]; STAGES],
            factor1: [[0.0; 4]; STAGES],
        }
    }
}

/// The register a four-bit source code names.
fn read_register(code: u32, registers: &Registers) -> [f32; 4] {
    match code & 0xF {
        1 => registers.constant0,
        2 => registers.constant1,
        3 => registers.fog,
        4 => registers.diffuse,
        5 => registers.specular,
        8 => registers.textures[0],
        9 => registers.textures[1],
        10 => registers.textures[2],
        11 => registers.textures[3],
        12 => registers.spare0,
        13 => registers.spare1,
        // `SPARE0 + SECONDARY` and `E·F` exist only in the final stage; a
        // general stage reading them sees zero, as an unnamed code does.
        _ => [0.0; 4],
    }
}

/// Applies one input's mapping to a value.
fn map_input(mapping: u32, value: f32) -> f32 {
    match mapping & 0x7 {
        0 => value.max(0.0),
        1 => 1.0 - value.clamp(0.0, 1.0),
        2 => value.max(0.0).mul_add(2.0, -1.0),
        3 => value.max(0.0).mul_add(-2.0, 1.0),
        4 => value.max(0.0) - 0.5,
        5 => 0.5 - value.max(0.0),
        6 => value,
        _ => -value,
    }
}

/// Reads one of a stage's four variables.
///
/// `field` is the byte describing it: the register in bits 0..4, whether
/// the alpha channel is read in bit 4, and the mapping in bits 5..8.
fn read_variable(field: u32, registers: &Registers, alpha_portion: bool) -> [f32; 4] {
    let value = read_register(field, registers);
    let mapping = field >> 5;
    if field & 0x10 != 0 || alpha_portion {
        // The alpha channel, broadcast: an alpha portion reads alpha from
        // every register, and a color portion may ask for it explicitly.
        let alpha = map_input(mapping, value[3]);
        return [alpha; 4];
    }
    [
        map_input(mapping, value[0]),
        map_input(mapping, value[1]),
        map_input(mapping, value[2]),
        map_input(mapping, value[3]),
    ]
}

/// The scale and bias a stage's output word asks for.
fn scale_and_bias(outputs: u32, value: f32) -> f32 {
    match (outputs >> 15) & 0x7 {
        1 => value - 0.5,
        2 => value * 2.0,
        3 => (value - 0.5) * 2.0,
        4 => value * 4.0,
        6 => value * 0.5,
        _ => value,
    }
}

/// Writes a result into the register a four-bit destination names.
fn write_register(code: u32, value: [f32; 4], registers: &mut Registers, alpha_only: bool) {
    let target = match code & 0xF {
        8 => &mut registers.textures[0],
        9 => &mut registers.textures[1],
        10 => &mut registers.textures[2],
        11 => &mut registers.textures[3],
        12 => &mut registers.spare0,
        13 => &mut registers.spare1,
        // Zero and the read-only registers discard what a stage writes.
        _ => return,
    };
    if alpha_only {
        target[3] = value[3];
    } else {
        target[0] = value[0];
        target[1] = value[1];
        target[2] = value[2];
    }
}

/// Runs one portion — color or alpha — of one general stage.
fn run_portion(inputs: u32, outputs: u32, registers: &mut Registers, alpha_portion: bool) {
    let a = read_variable(inputs >> 24, registers, alpha_portion);
    let b = read_variable((inputs >> 16) & 0xFF, registers, alpha_portion);
    let c = read_variable((inputs >> 8) & 0xFF, registers, alpha_portion);
    let d = read_variable(inputs & 0xFF, registers, alpha_portion);

    let dot = |left: [f32; 4], right: [f32; 4]| {
        let product = left[0].mul_add(right[0], left[1].mul_add(right[1], left[2] * right[2]));
        [product; 4]
    };
    let times = |left: [f32; 4], right: [f32; 4]| std::array::from_fn(|i| left[i] * right[i]);

    // A dot product replaces the pairwise product, and only the color
    // portion has one.
    let ab_dot = !alpha_portion && outputs & (1 << 13) != 0;
    let cd_dot = !alpha_portion && outputs & (1 << 12) != 0;
    let ab = if ab_dot { dot(a, b) } else { times(a, b) };
    let cd = if cd_dot { dot(c, d) } else { times(c, d) };

    let sum: [f32; 4] = if outputs & (1 << 14) != 0 {
        // The mux picks between the products by the spare register's alpha.
        let pick_cd = registers.spare0[3] >= 0.5;
        if pick_cd { cd } else { ab }
    } else {
        std::array::from_fn(|i| ab[i] + cd[i])
    };

    let scaled = |value: [f32; 4]| -> [f32; 4] {
        std::array::from_fn(|i| scale_and_bias(outputs, value[i]))
    };
    // The second product's destination comes first in the word, and a
    // dot product leaves no separate result to write.
    if !cd_dot {
        write_register(outputs, scaled(cd), registers, alpha_portion);
    }
    if !ab_dot {
        write_register(outputs >> 4, scaled(ab), registers, alpha_portion);
    }
    write_register(outputs >> 8, scaled(sum), registers, alpha_portion);
}

/// Runs the final stage, producing the pixel.
///
/// It computes `A·B + (1 - A)·C + D` for color, with `E·F` available as a
/// register, and takes alpha from `G`.
fn run_final(state: &CombinerState, registers: &mut Registers) -> [f32; 4] {
    let e = read_variable((state.final_second >> 24) & 0xFF, registers, false);
    let f = read_variable((state.final_second >> 16) & 0xFF, registers, false);
    let product: [f32; 4] = std::array::from_fn(|i| e[i] * f[i]);

    // The two registers only the final stage can read.
    let mut extended = *registers;
    extended.spare1 = std::array::from_fn(|i| registers.spare0[i] + registers.specular[i]);
    extended.textures[3] = product;

    let variable = |field: u32| -> [f32; 4] {
        let value = match field & 0xF {
            14 => extended.spare1,
            15 => product,
            _ => read_register(field, registers),
        };
        if field & 0x10 != 0 { [value[3]; 4] } else { value }
    };
    let inverted = |field: u32, value: [f32; 4]| -> [f32; 4] {
        if field & 0x20 != 0 { std::array::from_fn(|i| 1.0 - value[i]) } else { value }
    };

    let a_field = (state.final_first >> 24) & 0xFF;
    let b_field = (state.final_first >> 16) & 0xFF;
    let c_field = (state.final_first >> 8) & 0xFF;
    let d_field = state.final_first & 0xFF;
    let a = inverted(a_field, variable(a_field));
    let b = inverted(b_field, variable(b_field));
    let c = inverted(c_field, variable(c_field));
    let d = inverted(d_field, variable(d_field));

    let g_field = (state.final_second >> 8) & 0xFF;
    let g = variable(g_field);
    let color: [f32; 4] =
        std::array::from_fn(|i| a[i].mul_add(b[i], (1.0 - a[i]).mul_add(c[i], d[i])));
    [color[0], color[1], color[2], g[3]]
}

/// Runs the whole combiner, returning the pixel as 8-bit ARGB.
///
/// A configuration with no active stages and no final words is not a
/// program at all — a title that has not set one yet — and reports `None`
/// so the caller can fall back.
#[must_use]
pub fn evaluate(state: &CombinerState, registers: &Registers) -> Option<u32> {
    if state.active == 0 && state.final_first == 0 {
        return None;
    }
    let mut registers = *registers;
    for index in 0..state.active.min(STAGES) {
        let stage = state.stages[index];
        registers.constant0 = state.factor0[index];
        registers.constant1 = state.factor1[index];
        run_portion(stage.color_inputs, stage.color_outputs, &mut registers, false);
        run_portion(stage.alpha_inputs, stage.alpha_outputs, &mut registers, true);
    }
    let color = if state.final_first == 0 {
        // Without a final stage the last spare register is the pixel.
        registers.spare0
    } else {
        run_final(state, &mut registers)
    };
    let channel = |value: f32| ((value.clamp(0.0, 1.0) * 255.0 + 0.5) as u32) & 0xFF;
    Some(
        (channel(color[3]) << 24)
            | (channel(color[0]) << 16)
            | (channel(color[1]) << 8)
            | channel(color[2]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An input byte: register, whether alpha, and mapping.
    const fn input(register: u32, alpha: bool, mapping: u32) -> u32 {
        register | ((alpha as u32) << 4) | (mapping << 5)
    }

    /// A final-stage input byte: register, alpha, and the invert flag.
    const fn final_input(register: u32, alpha: bool, invert: bool) -> u32 {
        register | ((alpha as u32) << 4) | ((invert as u32) << 5)
    }

    /// The four input bytes of one portion, `A` highest.
    const fn inputs(a: u32, b: u32, c: u32, d: u32) -> u32 {
        (a << 24) | (b << 16) | (c << 8) | d
    }

    /// An output word: the two products' destinations — the second one
    /// first, as the hardware orders them — then the sum's.
    const fn outputs(ab: u32, cd: u32, sum: u32) -> u32 {
        cd | (ab << 4) | (sum << 8)
    }

    /// Registers with a red texture and a half-grey diffuse.
    fn registers() -> Registers {
        Registers {
            diffuse: [0.5, 0.5, 0.5, 1.0],
            textures: [[1.0, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0], [0.0; 4], [0.0; 4]],
            ..Registers::default()
        }
    }

    /// The common first stage: texture times diffuse into spare zero.
    fn modulate_stage() -> Stage {
        Stage {
            color_inputs: inputs(input(8, false, 0), input(4, false, 0), 0, 0),
            color_outputs: outputs(12, 0, 0),
            alpha_inputs: inputs(input(8, true, 0), input(4, true, 0), 0, 0),
            alpha_outputs: outputs(12, 0, 0),
        }
    }

    #[test]
    fn an_unprogrammed_combiner_reports_nothing() {
        assert!(evaluate(&CombinerState::default(), &registers()).is_none());
    }

    #[test]
    fn one_stage_modulates_a_texture_by_the_diffuse_color() {
        let mut state = CombinerState { active: 1, ..CombinerState::default() };
        state.stages[0] = modulate_stage();
        let color = evaluate(&state, &registers()).expect("the combiner runs");
        assert_eq!((color >> 16) & 0xFF, 0x80, "red is halved by the diffuse");
        assert_eq!(color & 0xFFFF, 0, "the texture has no green or blue");
    }

    #[test]
    fn the_sum_adds_both_products() {
        // AB = texture0 (red), CD = texture1 (green); their sum is yellow.
        let mut state = CombinerState { active: 1, ..CombinerState::default() };
        state.stages[0] = Stage {
            color_inputs: inputs(
                input(8, false, 0),
                input(0, false, 1),
                input(9, false, 0),
                input(0, false, 1),
            ),
            color_outputs: outputs(0, 0, 12),
            ..Stage::default()
        };
        let color = evaluate(&state, &registers()).expect("runs");
        assert_eq!((color >> 16) & 0xFF, 0xFF, "red from the first product");
        assert_eq!((color >> 8) & 0xFF, 0xFF, "green from the second");
    }

    #[test]
    fn an_inverted_input_reads_one_minus_the_value() {
        // A = 1 - diffuse (0.5) = 0.5, times white.
        let mut state = CombinerState { active: 1, ..CombinerState::default() };
        state.stages[0] = Stage {
            color_inputs: inputs(
                input(4, false, 1),
                input(0, false, 1),
                input(0, false, 0),
                input(0, false, 0),
            ),
            color_outputs: outputs(12, 0, 0),
            ..Stage::default()
        };
        let color = evaluate(&state, &registers()).expect("runs");
        assert_eq!((color >> 16) & 0xFF, 0x80, "one minus a half");
    }

    #[test]
    fn a_stage_output_can_be_scaled() {
        // Texture times white, doubled.
        let mut state = CombinerState { active: 1, ..CombinerState::default() };
        state.stages[0] = Stage {
            color_inputs: inputs(
                input(4, false, 0),
                input(0, false, 1),
                input(0, false, 0),
                input(0, false, 0),
            ),
            color_outputs: outputs(12, 0, 0) | (2 << 15),
            ..Stage::default()
        };
        let color = evaluate(&state, &registers()).expect("runs");
        assert_eq!((color >> 16) & 0xFF, 0xFF, "a half doubled saturates the channel");
    }

    #[test]
    fn a_dot_product_broadcasts_across_the_channels() {
        // texture0 . texture0 = 1, written to every color channel.
        let mut state = CombinerState { active: 1, ..CombinerState::default() };
        state.stages[0] = Stage {
            color_inputs: inputs(input(8, false, 0), input(8, false, 0), 0, 0),
            color_outputs: outputs(0, 0, 12) | (1 << 13),
            ..Stage::default()
        };
        let color = evaluate(&state, &registers()).expect("runs");
        assert_eq!(color & 0x00FF_FFFF, 0x00FF_FFFF, "a unit dot product is white");
    }

    #[test]
    fn stages_pass_results_through_the_spare_registers() {
        // Stage one puts the texture in spare zero; stage two halves it.
        let mut state = CombinerState { active: 2, ..CombinerState::default() };
        state.stages[0] = modulate_stage();
        state.stages[1] = Stage {
            color_inputs: inputs(
                input(12, false, 0),
                input(0, false, 1),
                input(0, false, 0),
                input(0, false, 0),
            ),
            color_outputs: outputs(12, 0, 0) | (6 << 15),
            ..Stage::default()
        };
        let color = evaluate(&state, &registers()).expect("runs");
        assert_eq!((color >> 16) & 0xFF, 0x40, "a half, halved again");
    }

    #[test]
    fn the_final_stage_blends_by_its_first_variable() {
        // A = 1 (inverted zero), so the result is B: the red texture.
        let mut state = CombinerState { active: 1, ..CombinerState::default() };
        state.stages[0] = modulate_stage();
        state.final_first = inputs(final_input(0, false, true), 8, 0, 0);
        state.final_second = (12 | 0x10) << 8;
        let color = evaluate(&state, &registers()).expect("runs");
        assert_eq!((color >> 16) & 0xFF, 0xFF, "the first variable selects B");
    }

    #[test]
    fn a_configuration_with_no_stages_but_a_final_word_still_runs() {
        let state = CombinerState {
            final_first: inputs(final_input(0, false, true), 4, 0, 0),
            final_second: (4 | 0x10) << 8,
            ..CombinerState::default()
        };
        let color = evaluate(&state, &registers()).expect("runs");
        assert_eq!((color >> 16) & 0xFF, 0x80, "the diffuse reaches the pixel");
    }
}
