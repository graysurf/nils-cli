# nils-docker-tools

`docker-tools` is the native Docker helper CLI migrated out of zsh-kit.

## Commands

```bash
docker-tools container sh <container>
docker-tools container zsh <container>
docker-tools container rm [--no-force] [-v|--volumes] <container...>
docker-tools compose down [--all] [--yes] [compose down args...]
docker-tools run zsh [--no-mount] [--workdir <path>] [--name <name>] [--user <user>|--root] <image>
docker-tools completion bash
docker-tools completion zsh
```

The shell-mutating `docker-tools alias ...` group remains a zsh-kit wrapper around
`docker-aliases`.

## Environment

- `ZSH_DOCKER_COMPOSE_CMD`: override compose command resolution, for example
  `docker compose` or `docker-compose`.
