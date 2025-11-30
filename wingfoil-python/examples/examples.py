from wingfoil import ticker, constant
import wingfoil

src = constant(7.0).sample(ticker(0.1))
n = src.count()
(
    bimap(src, n, lambda a, b: a + b * 1e-3)
        .logged(">>")
        .run(realtime=False, cycles=5)
)

