use ::std::pin::Pin;
use ::std::sync::Mutex;
use ::std::task::{Context, Poll};
use ::std::{future::Future, marker::PhantomPinned};

enum GeneratorStateInner<I, O> {
    Yielded(O),
    Fed(I),
    None,
}

impl<S, T> Default for GeneratorStateInner<S, T> {
    fn default() -> Self {
        Self::None
    }
}

impl<I, O> GeneratorStateInner<I, O> {
    #[inline]
    fn into_fed(self) -> Option<I> {
        match self {
            Self::Yielded(_) => None,
            Self::None => None,
            Self::Fed(v) => Some(v),
        }
    }
    #[inline]
    fn into_yielded(self) -> Option<O> {
        match self {
            Self::Yielded(v) => Some(v),
            Self::None => None,
            Self::Fed(_) => None,
        }
    }
    /// Move the value out of self if self is Fed
    fn take_fed(&mut self) -> Option<I> {
        match self {
            Self::Yielded(_) => None,
            Self::None => None,
            Self::Fed(_) => ::std::mem::replace(self, Self::None).into_fed(),
        }
    }
    fn take_yielded(&mut self) -> Option<O> {
        match self {
            Self::Fed(_) => None,
            Self::None => None,
            Self::Yielded(_) => ::std::mem::replace(self, Self::None).into_yielded(),
        }
    }
}

pub struct GeneratorState<I: 'static, O: 'static>(&'static Mutex<GeneratorStateInner<I, O>>);

pub struct GeneratorStateYield<'a, I: 'static, O: 'static>(&'a GeneratorState<I, O>);
impl<'a, I, O> Future for GeneratorStateYield<'a, I, O> {
    type Output = I;
    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        if let Some(v) = (self.0).0.lock().unwrap().take_fed() {
            Poll::Ready(v)
        } else {
            Poll::Pending
        }
    }
}

impl<I, O> GeneratorState<I, O> {
    pub(crate) fn yield_(&mut self, v: O) -> GeneratorStateYield<I, O> {
        *self.0.lock().unwrap() = GeneratorStateInner::Yielded(v);
        GeneratorStateYield(self)
    }
}

#[pin_project::pin_project(project = FutureOrFnProj)]
enum FutureOrFn<F1, F2> {
    Future(#[pin] F1),
    Func(Option<F2>),
}
#[pin_project::pin_project]
/// A future you have to manually drive to completion.
pub struct Generator<T: Future<Output = S>, S, I: 'static, O: 'static, F> {
    cell: Mutex<GeneratorStateInner<I, O>>,
    #[pin]
    future: FutureOrFn<T, F>,
    pinned: PhantomPinned,
}

use ::either::Either;
pub trait Turnable<I, O, S> {
    fn start(self: Pin<&mut Self>) -> Either<Option<O>, S>;
    fn turn(self: Pin<&mut Self>, feed: I) -> Either<O, S>;
}

impl<
        T: 'static + Future<Output = S>,
        S: 'static,
        I: 'static,
        O: 'static,
        F: FnOnce(GeneratorState<I, O>) -> T,
    > Generator<T, S, I, O, F>
{
    pub fn new(f: F) -> Self {
        Self {
            cell: Mutex::new(GeneratorStateInner::None),
            future: FutureOrFn::Func(Some(f)),
            pinned: PhantomPinned,
        }
    }

    fn turn_impl(self: Pin<&mut Self>) -> Either<Option<O>, S> {
        let self_ = self.project();

        match self_.future.project() {
            FutureOrFnProj::Future(fut) => {
                if let Poll::Ready(v) = fut.poll(unsafe { &mut *::std::ptr::null_mut() }) {
                    Either::Right(v)
                } else {
                    Either::Left(self_.cell.lock().unwrap().take_yielded())
                }
            }
            FutureOrFnProj::Func(_) => panic!("start() not called"),
        }
    }
}

impl<
        T: 'static + Future<Output = S>,
        S: 'static,
        I: 'static,
        O: 'static,
        F: FnOnce(GeneratorState<I, O>) -> T,
    > Turnable<I, O, S> for Generator<T, S, I, O, F>
{
    /// Must be called before the first `turn()`, returns Left(O) if the generator yields, Right(S)
    /// if the generator completes. If called multiple times, returns Left(None)
    fn start(mut self: Pin<&mut Self>) -> Either<Option<O>, S> {
        let self_ = self.as_mut().project();
        let future = unsafe { self_.future.get_unchecked_mut() };
        if let FutureOrFn::Func(f) = future {
            let f = f.take().unwrap();
            // The newly created future hasn't yet been pinned, so it's safe to move it
            *future = FutureOrFn::Future(f(GeneratorState(unsafe { &*(self_.cell as *const _) })));
            // Run the generator until it yields
            self.turn_impl()
        } else {
            Either::Left(None)
        }
    }

    fn turn(self: Pin<&mut Self>, feed: I) -> Either<O, S> {
        *self.as_ref().cell.lock().unwrap() = GeneratorStateInner::Fed(feed);
        match self.turn_impl() {
            Either::Left(v) => Either::Left(v.unwrap()),
            Either::Right(v) => Either::Right(v),
        }
    }
}

use ::std::marker::Unpin;
use ::std::ops::DerefMut;
impl<P: Unpin + DerefMut<Target = T>, T: Turnable<I, O, S> + ?Sized, I, O, S> Turnable<I, O, S>
    for Pin<P>
{
    fn start(self: Pin<&mut Self>) -> Either<Option<O>, S> {
        self.get_mut().as_mut().start()
    }
    fn turn(self: Pin<&mut Self>, feed: I) -> Either<O, S> {
        self.get_mut().as_mut().turn(feed)
    }
}

#[cfg(test)]
mod tests {
    use super::{Generator, GeneratorState, Turnable};
    use ::either::Either;
    use ::pin_utils::pin_mut;

    async fn read(mut c: GeneratorState<i32, ()>) -> i32 {
        let a = c.yield_(()).await;
        println!("A {}", a);
        let b = c.yield_(()).await;
        println!("B {}", b);
        a + b
    }
    #[test]
    fn test_async_cell() {
        let mf = Generator::new(read);
        pin_mut!(mf);
        assert_eq!(mf.as_mut().start(), Either::Left(Some(())));
        assert_eq!(mf.as_mut().turn(1), Either::Left(()));
        assert_eq!(mf.turn(2), Either::Right(3));
    }
}
