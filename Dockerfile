FROM python:3.11-slim

WORKDIR /app

COPY requirements.txt .
RUN pip install --no-cache-dir -r requirements.txt

COPY . .

RUN mkdir -p sandbox shared/tasks shared/results shared/mailbox shared/context logs

EXPOSE 8001 8002 8003 8004

CMD ["python", "launcher.py"]
