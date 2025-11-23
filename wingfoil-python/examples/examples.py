from wingfoil import ticker
import wingfoil

src = ticker(0.1).count()

strm = src.buffer(3).logged(">>")
strm.run(realtime=True, duration=2.0)

