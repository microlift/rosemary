//! Structures holding textures, samplers and buffer pointers, in the shape
//! Skia's SkSL-to-Metal output writes them.

use metal_translate::{TargetProfile, translate_msl};

fn words(source: &str) -> Vec<u32> {
    let translated = translate_msl(source, &TargetProfile::vulkan_1_3())
        .unwrap_or_else(|error| panic!("{error}\n{source}"));
    translated.entries[0].words.clone()
}

#[test]
fn a_texture_and_sampler_pair_samples_as_the_pair_itself() {
    let bundled = "struct sampler2D { texture2d<half> tex; sampler smp; };
        half4 sample(sampler2D i, float2 p, float b = -0.475) { return i.tex.sample(i.smp, p, bias(b)); }
        half4 sample(sampler2D i, float3 p, float b = -0.475) { return i.tex.sample(i.smp, p.xy / p.z, bias(b)); }
        struct Globals { sampler2D s; };
        fragment half4 probe(texture2d<half> t [[texture(0)]], sampler m [[sampler(0)]], float4 at [[position]]) {
            Globals _globals{{t, m}};
            (void)_globals;
            return sample(_globals.s, at.xy);
        }";
    // The same helper, given the pair as two parameters.
    let direct = "half4 sample(texture2d<half> tex, sampler smp, float2 p, float b = -0.475) { return tex.sample(smp, p, bias(b)); }
        fragment half4 probe(texture2d<half> t [[texture(0)]], sampler m [[sampler(0)]], float4 at [[position]]) {
            return sample(t, m, at.xy);
        }";
    assert_eq!(words(bundled), words(direct));
}

#[test]
fn a_pointer_member_reads_the_buffer_it_was_given() {
    let held = "struct U { float4 adjust; };
        struct Globals { constant U* u; };
        vertex float4 probe(constant U& u [[buffer(0)]]) {
            Globals _globals{&u};
            return _globals.u->adjust;
        }";
    let direct = "struct U { float4 adjust; };
        vertex float4 probe(constant U& u [[buffer(0)]]) { return u.adjust; }";
    assert_eq!(words(held), words(direct));
}

#[test]
fn a_handle_member_cannot_be_reassigned_or_left_unset() {
    let reassigned = "struct P { texture2d<half> tex; sampler smp; };
        fragment half4 probe(texture2d<half> t [[texture(0)]], texture2d<half> u [[texture(1)]], sampler m [[sampler(0)]]) {
            P p{t, m};
            p.tex = u;
            return p.tex.sample(p.smp, float2(0.0));
        }";
    let unset = "struct P { texture2d<half> tex; sampler smp; };
        fragment half4 probe() { P p; return half4(0.0); }";
    for source in [reassigned, unset] {
        assert!(
            translate_msl(source, &TargetProfile::vulkan_1_3()).is_err(),
            "{source}"
        );
    }
}

#[test]
fn a_buffer_of_handle_structures_is_refused_as_an_argument_buffer() {
    let source = "struct P { texture2d<half> tex; };
        fragment half4 probe(constant P& p [[buffer(0)]]) { return half4(0.0); }";
    let error = translate_msl(source, &TargetProfile::vulkan_1_3()).unwrap_err();
    assert!(error.to_string().contains("argument buffer"), "{error}");
}
