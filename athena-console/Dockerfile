FROM node:20-alpine AS frontend-builder
WORKDIR /app
COPY web/package.json web/package-lock.json* web/npm-shrinkwrap.json* web/yarn.lock* web/pnpm-lock.yaml* ./
RUN npm install
COPY web/ .
RUN npm run build
# Build backend
FROM golang:1.26-alpine AS backend-builder
WORKDIR /app
COPY api/go.mod api/go.sum* ./
RUN go mod download
COPY api/ .
RUN CGO_ENABLED=0 GOOS=linux go build -o athena-console .

FROM alpine:3.19
WORKDIR /app
COPY --from=backend-builder /app/athena-console .
COPY --from=frontend-builder /app/dist ./web/dist
EXPOSE 3000
ENV PORT=3000
CMD ["./athena-console"]
