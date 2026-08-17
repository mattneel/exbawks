//! The NV2A vertex program: decoding and execution.
//!
//! A title uploads a program of 128-bit instructions and runs it once per
//! vertex. Each instruction issues two operations at once — a vector one on
//! the multiply-accumulate unit and a scalar one on the inverse-logic unit
//! — reading up to three sources and writing a temporary register, an
//! output register, or both.
//!
//! The encoding is read off the words a retail title uploads: the fields
//! sit in the upper three of the four dwords, sources carry a type, index,
//! per-component swizzle and a negate bit, and the last instruction of a
//! program carries a final bit.

/// Temporary registers a program may use.
const TEMPORARY_REGISTERS: usize = 12;
/// Output registers a program may write.
const OUTPUT_REGISTERS: usize = 16;
/// Input (vertex attribute) registers a program may read.
pub const INPUT_REGISTERS: usize = 16;
/// The output register carrying clip-space position.
const OUTPUT_POSITION: usize = 0;
/// The output register carrying the diffuse color.
const OUTPUT_DIFFUSE: usize = 3;
/// The output register carrying the first texture coordinate set.
const OUTPUT_TEXCOORD0: usize = 9;
/// The most instructions one execution may run, so a malformed program
/// without a final bit cannot spin.
const MAX_STEPS: usize = 512;

/// A source operand's register type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceKind {
    /// A temporary register.
    Temporary,
    /// A vertex attribute.
    Input,
    /// A program constant.
    Constant,
}

/// What one execution produced.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShaderResult {
    /// Clip-space position, before the perspective divide.
    pub position: [f32; 4],
    /// The diffuse color, each channel in `0.0..=1.0`.
    pub diffuse: [f32; 4],
    /// The first texture coordinate set.
    pub texcoord0: [f32; 2],
}

/// Reads a field of `count` bits at `start` from one instruction word.
fn field(words: &[u32; 4], word: usize, start: u32, count: u32) -> u32 {
    (words[word] >> start) & ((1 << count) - 1)
}

/// Decodes one source operand, applying its swizzle and negation.
fn source(
    words: &[u32; 4],
    which: usize,
    temporaries: &[[f32; 4]],
    inputs: &[[f32; 4]],
    constants: &[[f32; 4]],
) -> [f32; 4] {
    let (negate, swizzle, register, kind) = match which {
        0 => (
            field(words, 1, 8, 1),
            [
                field(words, 1, 6, 2),
                field(words, 1, 4, 2),
                field(words, 1, 2, 2),
                field(words, 1, 0, 2),
            ],
            field(words, 2, 28, 4),
            field(words, 2, 26, 2),
        ),
        1 => (
            field(words, 2, 25, 1),
            [
                field(words, 2, 23, 2),
                field(words, 2, 21, 2),
                field(words, 2, 19, 2),
                field(words, 2, 17, 2),
            ],
            field(words, 2, 13, 4),
            field(words, 2, 11, 2),
        ),
        _ => (
            field(words, 2, 10, 1),
            [
                field(words, 2, 8, 2),
                field(words, 2, 6, 2),
                field(words, 2, 4, 2),
                field(words, 2, 2, 2),
            ],
            (field(words, 2, 0, 2) << 2) | field(words, 3, 30, 2),
            field(words, 3, 28, 2),
        ),
    };
    let kind = match kind {
        1 => SourceKind::Temporary,
        2 => SourceKind::Input,
        _ => SourceKind::Constant,
    };
    let value = match kind {
        SourceKind::Temporary => temporaries.get(register as usize).copied().unwrap_or([0.0; 4]),
        SourceKind::Input => {
            let index = field(words, 1, 9, 4) as usize;
            inputs.get(index).copied().unwrap_or([0.0; 4])
        }
        SourceKind::Constant => {
            let index = field(words, 1, 13, 8) as usize;
            constants.get(index).copied().unwrap_or([0.0; 4])
        }
    };
    let sign = if negate == 1 { -1.0 } else { 1.0 };
    [
        sign * value[swizzle[0] as usize],
        sign * value[swizzle[1] as usize],
        sign * value[swizzle[2] as usize],
        sign * value[swizzle[3] as usize],
    ]
}

/// Writes `value` into `target` through a four-bit mask, `x` first.
fn write_masked(target: &mut [f32; 4], value: [f32; 4], mask: u32) {
    for (index, component) in target.iter_mut().enumerate() {
        if mask & (8 >> index) != 0 {
            *component = value[index];
        }
    }
}

/// Runs the multiply-accumulate operation.
fn multiply_accumulate(opcode: u32, a: [f32; 4], b: [f32; 4], c: [f32; 4]) -> [f32; 4] {
    /// Broadcasts a scalar across four components.
    fn splat(value: f32) -> [f32; 4] {
        [value; 4]
    }
    match opcode {
        1 => a,                                                             // MOV
        2 => std::array::from_fn(|index| a[index] * b[index]),              // MUL
        3 => std::array::from_fn(|index| a[index] + c[index]),              // ADD
        4 => std::array::from_fn(|index| a[index] * b[index] + c[index]),   // MAD
        5 => splat(a[0] * b[0] + a[1] * b[1] + a[2] * b[2]),                // DP3
        6 => splat(a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + b[3]),         // DPH
        7 => splat(a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3]),  // DP4
        8 => [1.0, a[1] * b[1], a[2], b[3]],                                // DST
        9 => std::array::from_fn(|index| a[index].min(b[index])),           // MIN
        10 => std::array::from_fn(|index| a[index].max(b[index])),          // MAX
        11 => std::array::from_fn(|index| f32::from(a[index] < b[index])),  // SLT
        12 => std::array::from_fn(|index| f32::from(a[index] >= b[index])), // SGE
        _ => [0.0; 4],
    }
}

/// Runs the scalar operation, which reads only its third source.
fn scalar(opcode: u32, c: [f32; 4]) -> [f32; 4] {
    let value = c[0];
    let result = match opcode {
        1 => value, // MOV
        2 | 3 => {
            // RCP and RCC; a reciprocal of zero saturates rather than
            // producing an infinity the rasterizer would have to handle.
            if value == 0.0 { f32::MAX } else { 1.0 / value }
        }
        4 => {
            // RSQ
            if value == 0.0 { f32::MAX } else { 1.0 / value.abs().sqrt() }
        }
        5 => value.exp2(), // EXP
        6 => {
            // LOG
            if value == 0.0 { -f32::MAX } else { value.abs().log2() }
        }
        _ => 0.0,
    };
    [result; 4]
}

/// Executes a vertex program over one vertex's attributes.
///
/// Returns `None` when the program is empty, so a caller can fall back to
/// the geometry as supplied.
#[must_use]
pub fn execute(
    program: &[[u32; 4]],
    constants: &[[f32; 4]],
    inputs: &[[f32; 4]],
) -> Option<ShaderResult> {
    if program.is_empty() {
        return None;
    }
    let mut temporaries = [[0.0_f32; 4]; TEMPORARY_REGISTERS];
    let mut outputs = [[0.0_f32; 4]; OUTPUT_REGISTERS];
    let mut executed = false;

    for words in program.iter().take(MAX_STEPS) {
        let vector_opcode = field(words, 1, 21, 4);
        let scalar_opcode = field(words, 1, 25, 3);
        let final_instruction = field(words, 3, 0, 1) == 1;
        if vector_opcode == 0 && scalar_opcode == 0 && !final_instruction {
            // An empty slot before the end is padding, not the program.
            if executed {
                break;
            }
            continue;
        }
        executed = true;

        let a = source(words, 0, &temporaries, inputs, constants);
        let b = source(words, 1, &temporaries, inputs, constants);
        let c = source(words, 2, &temporaries, inputs, constants);
        let vector = multiply_accumulate(vector_opcode, a, b, c);
        let scalar_result = scalar(scalar_opcode, c);

        let temporary = field(words, 3, 20, 4) as usize;
        if let Some(register) = temporaries.get_mut(temporary) {
            if vector_opcode != 0 {
                write_masked(register, vector, field(words, 3, 24, 4));
            }
            if scalar_opcode != 0 {
                write_masked(register, scalar_result, field(words, 3, 16, 4));
            }
        }

        let output_mask = field(words, 3, 12, 4);
        if output_mask != 0 {
            let value = if field(words, 3, 2, 1) == 1 { scalar_result } else { vector };
            let address = field(words, 3, 3, 8) as usize;
            let to_output = field(words, 3, 11, 1) == 1;
            let target =
                if to_output { outputs.get_mut(address) } else { temporaries.get_mut(address) };
            if let Some(target) = target {
                write_masked(target, value, output_mask);
            }
        }

        if final_instruction {
            break;
        }
    }

    executed.then(|| ShaderResult {
        position: outputs[OUTPUT_POSITION],
        diffuse: outputs[OUTPUT_DIFFUSE],
        texcoord0: [outputs[OUTPUT_TEXCOORD0][0], outputs[OUTPUT_TEXCOORD0][1]],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds one instruction from its fields, mirroring the encoding.
    #[derive(Default)]
    struct Instruction {
        vector: u32,
        scalar: u32,
        input: u32,
        constant: u32,
        a: (u32, u32),
        b: (u32, u32),
        c: (u32, u32),
        temporary: u32,
        vector_mask: u32,
        scalar_mask: u32,
        output_mask: u32,
        output_address: u32,
        output_from_scalar: bool,
        to_output: bool,
        final_instruction: bool,
    }

    /// The identity swizzle, `xyzw`.
    const IDENTITY: u32 = 0x1B;

    impl Instruction {
        fn encode(&self) -> [u32; 4] {
            let mut words = [0_u32; 4];
            words[1] = (self.scalar << 25)
                | (self.vector << 21)
                | (self.constant << 13)
                | (self.input << 9)
                | IDENTITY;
            words[2] = (self.a.1 << 28)
                | (self.a.0 << 26)
                | (IDENTITY << 17)
                | (self.b.1 << 13)
                | (self.b.0 << 11)
                | (IDENTITY << 2)
                | ((self.c.1 >> 2) & 0x3);
            words[3] = ((self.c.1 & 0x3) << 30)
                | (self.c.0 << 28)
                | (self.vector_mask << 24)
                | (self.temporary << 20)
                | (self.scalar_mask << 16)
                | (self.output_mask << 12)
                | (u32::from(self.to_output) << 11)
                | (self.output_address << 3)
                | (u32::from(self.output_from_scalar) << 2)
                | u32::from(self.final_instruction);
            words
        }
    }

    fn inputs() -> Vec<[f32; 4]> {
        let mut inputs = vec![[0.0; 4]; INPUT_REGISTERS];
        inputs[0] = [2.0, 3.0, 4.0, 1.0];
        inputs[9] = [0.25, 0.75, 0.0, 0.0];
        inputs
    }

    #[test]
    fn a_move_to_the_position_output_runs() {
        // MOV o0, v0 — source A is the input register, written to output 0.
        let program = [Instruction {
            vector: 1,
            input: 0,
            a: (2, 0),
            output_mask: 0xF,
            output_address: 0,
            to_output: true,
            final_instruction: true,
            ..Instruction::default()
        }
        .encode()];
        let result = execute(&program, &[[0.0; 4]; 4], &inputs()).expect("the program runs");
        assert_eq!(result.position, [2.0, 3.0, 4.0, 1.0]);
    }

    #[test]
    fn a_multiply_and_add_scales_and_biases() {
        // MAD o0, v0, c0, c1 — the shape a pre-transformed vertex takes.
        let constants = vec![[0.5, 0.5, 1.0, 1.0], [1.0, 2.0, 0.0, 0.0], [0.0; 4]];
        let program = [Instruction {
            vector: 4,
            input: 0,
            constant: 0,
            a: (2, 0),
            b: (3, 0),
            c: (3, 1),
            output_mask: 0xF,
            to_output: true,
            final_instruction: true,
            ..Instruction::default()
        }
        .encode()];
        // Source C reads constant 1 only if the constant field allows it;
        // the encoding carries one constant index, so this reads c0 twice.
        let result = execute(&program, &constants, &inputs()).expect("the program runs");
        assert_eq!(result.position[0], 2.0 * 0.5 + 0.5, "a * b + c on the x lane");
    }

    #[test]
    fn a_temporary_carries_between_instructions() {
        // MOV r1, v0 then MOV o0, r1.
        let first = Instruction {
            vector: 1,
            input: 0,
            a: (2, 0),
            temporary: 1,
            vector_mask: 0xF,
            ..Instruction::default()
        }
        .encode();
        let second = Instruction {
            vector: 1,
            a: (1, 1),
            output_mask: 0xF,
            to_output: true,
            final_instruction: true,
            ..Instruction::default()
        }
        .encode();
        let result = execute(&[first, second], &[[0.0; 4]; 4], &inputs()).expect("runs");
        assert_eq!(result.position, [2.0, 3.0, 4.0, 1.0]);
    }

    #[test]
    fn a_dot_product_broadcasts_its_result() {
        // DP4 o0, v0, v0 — the square of the input's length.
        let program = [Instruction {
            vector: 7,
            input: 0,
            a: (2, 0),
            b: (2, 0),
            output_mask: 0xF,
            to_output: true,
            final_instruction: true,
            ..Instruction::default()
        }
        .encode()];
        let result = execute(&program, &[[0.0; 4]; 4], &inputs()).expect("runs");
        assert_eq!(result.position, [30.0; 4], "4 + 9 + 16 + 1 in every lane");
    }

    #[test]
    fn the_scalar_unit_writes_its_own_mask() {
        // RCP r0.x from c0, moved to the position output's x lane.
        let constants = vec![[4.0, 0.0, 0.0, 0.0]];
        let program = [Instruction {
            scalar: 2,
            constant: 0,
            c: (3, 0),
            temporary: 0,
            scalar_mask: 0x8,
            output_mask: 0x8,
            output_from_scalar: true,
            to_output: true,
            final_instruction: true,
            ..Instruction::default()
        }
        .encode()];
        let result = execute(&program, &constants, &inputs()).expect("runs");
        assert_eq!(result.position[0], 0.25, "the reciprocal of four");
    }

    #[test]
    fn texture_coordinates_reach_their_output() {
        // MOV o9, v9 — the first texture coordinate set.
        let program = [Instruction {
            vector: 1,
            input: 9,
            a: (2, 0),
            output_mask: 0xF,
            output_address: 9,
            to_output: true,
            final_instruction: true,
            ..Instruction::default()
        }
        .encode()];
        let result = execute(&program, &[[0.0; 4]; 4], &inputs()).expect("runs");
        assert_eq!(result.texcoord0, [0.25, 0.75]);
    }

    #[test]
    fn an_empty_program_reports_nothing() {
        assert!(execute(&[], &[[0.0; 4]; 4], &inputs()).is_none());
        assert!(execute(&[[0; 4]], &[[0.0; 4]; 4], &inputs()).is_none());
    }

    #[test]
    fn a_program_without_a_final_bit_still_terminates() {
        // Every slot writes the output; none is marked final.
        let instruction = Instruction {
            vector: 1,
            input: 0,
            a: (2, 0),
            output_mask: 0xF,
            to_output: true,
            ..Instruction::default()
        }
        .encode();
        let program = vec![instruction; MAX_STEPS * 2];
        let result = execute(&program, &[[0.0; 4]; 4], &inputs()).expect("runs");
        assert_eq!(result.position, [2.0, 3.0, 4.0, 1.0]);
    }
}
