# elements

this is a list of what keys you are able to define for services, whether they are user-editable, and their quadlet equivalent

## editable

- command (Exec) (value only)
- container_name (ContainerName) (value only)
- devices (AddDevice) (value only)
- dns (DNS) (value only)
- dns_opt (DNSOption)
- dns_search (DNSSearch) (value only)
- environment (Environment) 
- gpus (AddDevice) (value only)
- group_add (GroupAdd) (value only)
- healthcheck (Health\*) (value only)
- hostname (HostName) (value only)
- image (Image) (special)
- logging (LogDriver and LogOpt) (value and key)
- mem_limit (Memory) (value only)
- network_mode (Network, not consistent with compose spec)
- networks (Network)
- ports (PublishPort nd HostPort) (value only)
- privileged (this one is tricky) (value only, boolean)
- pull_policy (Pull) (value only)
- read_only (ReadOnly) (value only, bool)
- restart (also tricky) (value only)
- secrets (Secret) (value only, reads filename from elsewhere)
- ulimits (Ulimit) (key and value)
- volumes (Volume) (key and value)
- working_dir (WorkingDir) (value only)

## non-editable

- annotations (Annotation) (value only)
- cap_add (AddCapability) (value only)
- cap_drop (DropCapability) (value only)
- cgroup (CgroupsMode) (value only)
- entrypoint (Entrypoint) (value only)
- extra_hosts (AddHost) (key and value)
- init (RunInit) (value only, bool)
- labels (Label) (value only)
- pids_limit (PidsLimit) (value only)
- security_opt (SecurityLabel{Disable, FileType, Level, Nested, Type} only) (key and value)
- shm_size (ShmSize) (value only)
- stop_grace_period (StopTimeout) (value only)
- stop_signal (StopSignal) (value only)
- sysctls (Sysctl) (key and value)
- tmpfs (Tmpfs) (value only)
- user (User) (value only)
- userns_mode (UserNS) (key and value)
- uts (PodmanArgs --uts) (value only, boolean)

## not present in compose, present in tetra

- AutoUpdate (autoupdate) (value only)
- ContainersConfModule (module) (value only)
- ExposeHostPort
- Group (id) (group) (value only)
- Notify (notify) (boolean)
- PidsLimit (pids_limit) (value only)
- Pod (pod) (value only)
- ReloadCmd (reload_cmd) (value only)
- ReloadSignal (reload_signal) (value only)
- Retry (retry) (value only)
- RetryDelay (retry_delay) (value only)
- StartWithPod (start_with_pod) (boolean)
- SubGIDMap (sub_gid_map) (value only)
- SubUIDMap (sub_uid_map) (value only)
- Timezone (timezone) (value only)
- UIDMap (sub_uid_map) (value only)

## not present in tetra

- cpu_count
- cpu_percent
- cpu_shares
- cpus
- attach
- build
- blkio_config
- cpuset
- cgroup_parent
- configs
- credential_spec
- depends_on
  - use unit section for this
- deploy
- develop
- device_cgroup_rules
- env_file (EnvironmentFile)
- domainname
- use hostname
- expose
- extends
  - use unit section
- external_links
  - use unit section
- ipc
- label_file
  - define directly in yaml
- links
- mac_address
  - use networks.mac_address
- mem_reservation
- mem_swappiness
- memswap_limit
- models
- pid
- platform
- pre_stop
- profiles
- provider
- runtime
- scale
- stdin_open
- storage_opt
- tty
- use_api_socket
- volumes_from
