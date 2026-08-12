FROM ubuntu:22.04

RUN apt-get update && apt-get install -y libssl3 && rm -rf /var/lib/apt/lists/*

WORKDIR /q
COPY q q.k ./
RUN chmod +x q

ENV QHOME=/q
EXPOSE 5000

CMD ["./q", "-p", "5000"]
