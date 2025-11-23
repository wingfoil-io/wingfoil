from wingfoil import ticker, constant
import wingfoil


strm = constant(7.0).sample(ticker(0.1)).logged(">>")
strm.run(realtime=False, duration=2.0)

