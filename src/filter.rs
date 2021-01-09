use std::f64::consts::PI;

use dasp::{Sample, Frame};
use dasp::sample::ToSample;

#[cfg(test)] use approx::AbsDiffEq;

#[derive(Copy, Clone, Debug)]
enum Kind {
    Shelving, HighPass,
}

impl Kind {
    fn coefficients(&self, sample_rate: u32) -> Coefficients {
        let (f0, q) =
            match self {
                Self::Shelving => (1681.974450955533, 0.7071752369554196),
                Self::HighPass => (38.13547087602444, 0.5003270373238773),
            }
        ;

        let k = (PI * f0 / sample_rate as f64).tan();
        let k_by_q = k / q;
        let k_sq = k * k;

        let a0 = 1.0 + k_by_q + k_sq;
        let a1 = 2.0 * (k_sq - 1.0) / a0;
        let a2 = (1.0 - k_by_q + k_sq) / a0;

        let (b0, b1, b2) =
            match self {
                Self::Shelving => {
                    let height = 3.999843853973347;

                    let vh = 10.0f64.powf(height / 20.0);
                    let vb = vh.powf(0.4996667741545416);

                    let b0 = (vh + vb * k_by_q + k_sq) / a0;
                    let b1 = 2.0 * (k_sq - vh) / a0;
                    let b2 = (vh - vb * k_by_q + k_sq) / a0;

                    (b0, b1, b2)
                },
                Self::HighPass => (1.0, -2.0, 1.0),
            }
        ;

        Coefficients { a1, a2, b0, b1, b2, }
    }
}

/// Coefficients for a biquad digital filter at a particular sample rate.
/// It is assumed that the `a0` coefficient is always normalized to 1.0,
/// and thus not included here.
#[derive(Copy, Clone, Debug, PartialEq)]
struct Coefficients {
    // Numerator coefficients.
    b0: f64,
    b1: f64,
    b2: f64,

    // Denominator coefficients, a0 is implied/assumed to be normalized to 1.0.
    a1: f64,
    a2: f64,
}

#[cfg(test)]
impl AbsDiffEq for Coefficients {
    type Epsilon = f64;

    fn default_epsilon() -> Self::Epsilon {
        f64::default_epsilon()
    }

    fn abs_diff_eq(&self, other: &Self, epsilon: Self::Epsilon) -> bool {
        f64::abs_diff_eq(&self.a1, &other.a1, epsilon)
            && f64::abs_diff_eq(&self.a2, &other.a2, epsilon)
            && f64::abs_diff_eq(&self.b0, &other.b0, epsilon)
            && f64::abs_diff_eq(&self.b1, &other.b1, epsilon)
            && f64::abs_diff_eq(&self.b2, &other.b2, epsilon)
    }
}

// TODO: Clean this up when const generics are stabilized.
#[derive(Copy, Clone, Debug)]
struct FilterPass<F: Frame<Sample = f64>> {
    coeff: Coefficients,
    m1: F,
    m2: F,
}

impl<F: Frame<Sample = f64>> FilterPass<F> {
    fn from_coeff(coeff: Coefficients) -> Self {
        Self {
            coeff,
            m1: F::EQUILIBRIUM,
            m2: F::EQUILIBRIUM,
        }
    }

    fn from_kind(kind: Kind, sample_rate: u32) -> Self {
        Self::from_coeff(kind.coefficients(sample_rate))
    }

    pub fn apply<I>(&mut self, input: &I) -> F
    where
        I: Frame<NumChannels = F::NumChannels>,
        I::Sample: ToSample<f64>
    {
        // Copy and convert to f64.
        let input: F = (*input).map(|x| x.to_sample::<f64>());

        // https://www.earlevel.com/main/2012/11/26/biquad-c-source-code/
        // https://github.com/korken89/biquad-rs/blob/master/src/lib.rs
        let co = &self.coeff;

        // Note that `offset_amp`/`scale_amp` are scalar addition/product, and
        // `add_amp`/`mul_amp` are vector addition/product.
        let out = self.m1.add_amp(input.scale_amp(co.b0));
        self.m1 = self.m2.add_amp(input.scale_amp(co.b1).add_amp(out.scale_amp(-co.a1)));
        self.m2 = input.scale_amp(co.b2).add_amp(out.scale_amp(-co.a2));

        out
    }
}

/// The initial two-pass "K"-filter as described by the ITU-R BS.1770-4 spec.
/// The first pass is a high shelf boost filter, which accounts for the acoustic
/// effects of the listener's head, assumed to be roughly spherical. The second
/// pass is a simple high pass filter.
#[derive(Copy, Clone, Debug)]
struct Filter<F: Frame<Sample = f64>> {
    pass_a: FilterPass<F>,
    pass_b: FilterPass<F>,
}

impl<F: Frame<Sample = f64>> Filter<F> {
    pub fn new(sample_rate: u32) -> Self {
        let pass_a = FilterPass::from_kind(Kind::Shelving, sample_rate);
        let pass_b = FilterPass::from_kind(Kind::HighPass, sample_rate);

        Self { pass_a, pass_b, }
    }

    pub fn apply<I>(&mut self, input: &I) -> F
    where
        I: Frame<NumChannels = F::NumChannels>,
        I::Sample: ToSample<f64>
    {
        self.pass_b.apply(&self.pass_a.apply(input))
    }
}

/// Iterator that peforms the K-weighted filtering step on each sample in an
/// iterable.
pub struct FilteredSamples<F, I>
where
    F: Frame<Sample = f64>,
    I: Iterator,
    I::Item: Frame<NumChannels = F::NumChannels>,
    <I::Item as Frame>::Sample: ToSample<f64>,
{
    samples: I,
    filter: Filter<F>,
}

impl<F, I> FilteredSamples<F, I>
where
    F: Frame<Sample = f64>,
    I: Iterator,
    I::Item: Frame<NumChannels = F::NumChannels>,
    <I::Item as Frame>::Sample: ToSample<f64>,
{
    pub fn new<II>(samples: II, sample_rate: u32) -> Self
    where
        II: IntoIterator<IntoIter = I, Item = I::Item>,
    {
        let filter = Filter::new(sample_rate);

        Self { samples: samples.into_iter(), filter }
    }
}

impl<F, I> Iterator for FilteredSamples<F, I>
where
    F: Frame<Sample = f64>,
    I: Iterator,
    I::Item: Frame<NumChannels = F::NumChannels>,
    <I::Item as Frame>::Sample: ToSample<f64>,
{
    type Item = F;

    fn next(&mut self) -> Option<Self::Item> {
        let raw_sample = self.samples.next()?;
        let filtered_sample = self.filter.apply(&raw_sample);

        Some(filtered_sample)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.samples.size_hint()
    }
}

impl<F, I> ExactSizeIterator for FilteredSamples<F, I>
where
    F: Frame<Sample = f64>,
    I: Iterator + ExactSizeIterator,
    I::Item: Frame<NumChannels = F::NumChannels>,
    <I::Item as Frame>::Sample: ToSample<f64>,
{
    fn len(&self) -> usize {
        self.samples.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::process::Command;

    use approx::{abs_diff_eq, assert_abs_diff_eq};

    use crate::test_util::{TestUtil, WaveKind};

    #[test]
    fn coefficients() {
        let expected = Coefficients {
            a1: -1.6906592931824103,
            a2: 0.7324807742158501,
            b0: 1.5351248595869702,
            b1: -2.6916961894063807,
            b2: 1.19839281085285,
        };
        let produced = Kind::Shelving.coefficients(48000);

        assert_abs_diff_eq!(expected, produced);

        let expected = Coefficients {
            a1: -1.6636551132560204,
            a2: 0.7125954280732254,
            b0: 1.5308412300503478,
            b1: -2.6509799951547297,
            b2: 1.169079079921587,
        };
        let produced = Kind::Shelving.coefficients(44100);

        assert_abs_diff_eq!(expected, produced);

        let expected = Coefficients {
            a1: -0.2933807824149212,
            a2: 0.18687510604540827,
            b0: 1.3216235689299776,
            b1: -0.7262554913156911,
            b2: 0.2981262460162007,
        };
        let produced = Kind::Shelving.coefficients(8000);

        assert_abs_diff_eq!(expected, produced);

        let expected = Coefficients {
            a1: -1.9222022306074886,
            a2: 0.925117735116826,
            b0: 1.5722272150912788,
            b1: -3.0472830515615508,
            b2: 1.4779713409796091,
        };
        let produced = Kind::Shelving.coefficients(192000);

        assert_abs_diff_eq!(expected, produced);

        let expected = Coefficients {
            a1: -1.99004745483398,
            a2:  0.99007225036621,
            b0:  1.00000000000000,
            b1: -2.00000000000000,
            b2:  1.00000000000000,
        };
        let produced = Kind::HighPass.coefficients(48000);

        assert_abs_diff_eq!(expected, produced);
    }

    #[test]
    fn filter_pass_apply() {
        let mut filter_pass: FilterPass<[_; 5]> = FilterPass::from_kind(Kind::Shelving, 48000);

        let expected_rows = vec![
            [-1.5351248595869702, -0.7675624297934851, 0.0, 0.7675624297934851, 1.5351248595869702],
            [-1.4388017802366435, -0.7194008901183218, 0.0, 0.7194008901183218, 1.4388017802366435],
            [-1.3498956361696552, -0.6749478180848276, 0.0, 0.6749478180848276, 1.3498956361696552],
            [-1.2701404412191692, -0.6350702206095846, 0.0, 0.6350702206095846, 1.2701404412191692],
            [-1.2004236209352888, -0.6002118104676444, 0.0, 0.6002118104676444, 1.2004236209352888],
            [-1.1409753777762859, -0.5704876888881429, 0.0, 0.5704876888881429, 1.1409753777762859],
            [-1.0915348835135539, -0.5457674417567769, 0.0, 0.5457674417567769, 1.0915348835135539],
            [-1.0514925476036132, -0.5257462738018066, 0.0, 0.5257462738018066, 1.0514925476036132],
        ];

        let input = [-1.0, -0.5, 0.0, 0.5, 1.0];

        for expected in expected_rows {
            let produced = filter_pass.apply(&input);

            for (i, (px, ex)) in produced.iter().zip(&expected).enumerate() {
                assert!(
                    abs_diff_eq!(px, ex, epsilon = 1e-9),
                    "samples @ {} differ: {} != {}", i, px, ex
                );
            }
        }
    }

    #[test]
    fn filter_apply() {
        let mut filter = Filter::<[_; 5]>::new(48000);

        let expected_rows = vec![
            [-1.5351248595869702, -0.7675624297934851, 0.0, 0.7675624297934851, 1.5351248595869702],
            [-1.4235233807361238, -0.7117616903680619, 0.0, 0.7117616903680619, 1.4235233807361238],
            [-1.3204114916895402, -0.6602057458447701, 0.0, 0.6602057458447701, 1.3204114916895402],
            [-1.2274414804724807, -0.6137207402362403, 0.0, 0.6137207402362403, 1.2274414804724807],
            [-1.1454023918520506, -0.5727011959260253, 0.0, 0.5727011959260253, 1.1454023918520506],
            [-1.0744179430265826, -0.5372089715132913, 0.0, 0.5372089715132913, 1.0744179430265826],
            [-1.0141193181684820, -0.5070596590842410, 0.0, 0.5070596590842410, 1.014119318168482],
            [-0.9637923356857869, -0.48189616784289346, 0.0, 0.48189616784289346, 0.9637923356857869],
        ];

        let input = [-1.0, -0.5, 0.0, 0.5, 1.0];

        for expected in expected_rows {
            let produced = filter.apply(&input);

            for (i, (px, ex)) in produced.iter().zip(&expected).enumerate() {
                assert!(
                    abs_diff_eq!(px, ex, epsilon = 1e-9),
                    "samples @ {} differ: {} != {}", i, px, ex
                );
            }
        }
    }

    fn sox_gen_wave_filtered_cmd(sample_rate: u32, kind: &WaveKind, frequency: u32) -> Command {
        let mut cmd = TestUtil::sox_gen_wave_cmd(sample_rate, kind, frequency);

        // Shelving filter.
        let coeff = Kind::Shelving.coefficients(sample_rate);
        cmd.arg("biquad")
            .arg(coeff.b0.to_string())
            .arg(coeff.b1.to_string())
            .arg(coeff.b2.to_string())
            .arg("1.0")
            .arg(coeff.a1.to_string())
            .arg(coeff.a2.to_string())
        ;

        // High pass filter.
        let coeff = Kind::HighPass.coefficients(sample_rate);
        cmd.arg("biquad")
            .arg(coeff.b0.to_string())
            .arg(coeff.b1.to_string())
            .arg(coeff.b2.to_string())
            .arg("1.0")
            .arg(coeff.a1.to_string())
            .arg(coeff.a2.to_string())
        ;

        cmd
    }

    #[test]
    fn sox_filter_suite() {
        const RATE: u32 = 48000;
        const KIND: &WaveKind = &WaveKind::Sine;
        const FREQ: u32 = 997;

        let samples = TestUtil::sox_eval_samples(&mut TestUtil::sox_gen_wave_cmd(RATE, KIND, FREQ))
            .into_iter()
            .map(|x| [x, 0.0, 0.0, 0.0, 0.0]);

        let filtered_samples = FilteredSamples::<[_; 5], _>::new(samples, 48000).map(|s| s[0]);

        let fx = TestUtil::sox_eval_samples(&mut sox_gen_wave_filtered_cmd(RATE, KIND, FREQ));

        // Check that the number of samples stays the same.
        assert_eq!(
            filtered_samples.len(), fx.len(),
            "sample counts differ: {} != {}", filtered_samples.len(), fx.len(),
        );

        for (i, (px, ex)) in filtered_samples.zip(fx).enumerate() {
            assert!(
                abs_diff_eq!(px, ex, epsilon = 1e-9),
                "samples @ {} differ: {} != {}", i, px, ex
            );
        }
    }
}

