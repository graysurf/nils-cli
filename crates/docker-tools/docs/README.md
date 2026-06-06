# docker-tools docs

`docker-tools` owns Docker helper behavior that does not need to mutate the
current shell. zsh-kit should call this binary from thin wrappers and keep only
alias mutation in shell code.
