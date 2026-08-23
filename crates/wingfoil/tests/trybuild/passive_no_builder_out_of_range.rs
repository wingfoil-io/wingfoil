use wingfoil::anyhow::Result;
use wingfoil::op;
use wingfoil::op::{Activation, Ctx, Op, Tick};

struct InvalidPassive;

#[op(build = invalid_passive, no_builder, passive = [1])]
impl Op for InvalidPassive {
    type Cfg = ();
    type State = ();
    type In<'a> = (&'a u64,);
    type Out = u64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        _state: &mut (),
        input: (&u64,),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<u64>> {
        Ok(Tick::Value(*input.0))
    }
}

fn main() {}
