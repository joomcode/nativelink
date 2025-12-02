FROM python:3.12-alpine

LABEL org.opencontainers.image.title="NativeLink Scheduler Dashboard"
LABEL org.opencontainers.image.description="Web dashboard for monitoring NativeLink scheduler state"

WORKDIR /app

# Copy dashboard files
COPY index.html server.py ./

# Default configuration
ENV PORT=8080
ENV SCHEDULER_URL=http://localhost:50051

EXPOSE 8080

CMD ["python3", "server.py"]

