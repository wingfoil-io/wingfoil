from wingfoil import ticker, constant, bimap
import wingfoil

period = 0.1

a = ticker(period * 2).count()
b = constant(0.7).sample(ticker(period))
(
    bimap(a, b, lambda a, b: a + b)
        .logged(">>")
        .run(realtime=False, duration=1.0)
)

