use crate::types::*;
use derive_new::new;
use std::rc::Rc;

#[derive(new)]
pub struct FinallyNode<T: Element, F: FnOnce(T, &GraphState) -> anyhow::Result<()>> {
    source: Rc<dyn Stream<T>>,
    finally: Option<F>,
    #[new(default)]
    value: T,
}

#[node(active = [source])]
impl<T: Element, F: FnOnce(T, &GraphState) -> anyhow::Result<()>> MutableNode
    for FinallyNode<T, F>
{
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        self.value = self.source.peek_value();
        Ok(true)
    }
    fn stop(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
        if let Some(f) = self.finally.take() {
            f(self.value.clone(), state)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::graph::*;
    use crate::nodes::*;
    use crate::types::NanoTime;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn finally_fires_after_graph_stops() {
        let src: Rc<RefCell<CallBackStream<u64>>> = Rc::new(RefCell::new(CallBackStream::new()));
        src.borrow_mut().push(ValueAt::new(99, NanoTime::new(100)));

        let fired = Rc::new(RefCell::new(false));
        let fired2 = fired.clone();
        let node = src.clone().as_stream().finally(move |val, _state| {
            *fired2.borrow_mut() = true;
            assert_eq!(val, 99);
            Ok(())
        });
        node.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
            .unwrap();
        assert!(*fired.borrow());
    }

    #[test]
    fn finally_error_propagates() {
        let src: Rc<RefCell<CallBackStream<u64>>> = Rc::new(RefCell::new(CallBackStream::new()));
        src.borrow_mut().push(ValueAt::new(1, NanoTime::new(100)));

        let node = src
            .clone()
            .as_stream()
            .finally(|_val, _state| Err(anyhow::anyhow!("stop error")));
        let result = node.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever);
        assert!(result.is_err());
    }

    #[test]
    fn finally_fires_with_default_if_never_ticked() {
        let src: Rc<RefCell<CallBackStream<u64>>> = Rc::new(RefCell::new(CallBackStream::new()));
        // no values pushed — node never ticks
        let fired = Rc::new(RefCell::new(false));
        let fired2 = fired.clone();
        let node = src.clone().as_stream().finally(move |val, _state| {
            *fired2.borrow_mut() = true;
            assert_eq!(val, 0); // default for u64
            Ok(())
        });
        node.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
            .unwrap();
        assert!(*fired.borrow());
    }
}
