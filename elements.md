# elements

this is a list of what keys you are able to define for services, whether they are user-editable, and their quadlet equivalent

## editable

- command (Exec)
- container_name (ContainerName)
- devices (AddDevice)
- dns (DNS)
- dns_opt (DNSOption)
- dns_search (DNSSearch)
- environment (Environment)
- gpus (AddDevice)
- group_add (GroupAdd)
- healthcheck (Health\*)
- hostname (HostName)
- image (Image)
- logging (LogDriver and LogOpt)
- mem_limit (Memory)
- network_mode (Network, not consistent with compose spec)
- networks (Network)
- ports (PublishPort nd HostPort)
- privileged (this one is tricky)
- pull_policy (Pull)
- read_only (ReadOnly)
- restart (also tricky)
- secrets (Secret)
- ulimits (Ulimit)
- volumes (Volume)
- working_dir (WorkingDir)

## non-editable

- annotations (Annotation)
- cap_add (AddCapability)
- cap_drop (DropCapability)
- cgroup (CgroupsMode)
- entrypoint (Entrypoint)
- extra_hosts (AddHost)
- init (RunInit)
- labels (Label)
- pids_limit (PidsLimit)
- security_opt (SecurityLabel{Disable, FileType, Level, Nested, Type} only)
- shm_size (ShmSize)
- stop_grace_period (StopTimeout)
- stop_signal (StopSignal)
- sysctls (Sysctl)
- tmpfs (Tmpfs)
- user (User)
- userns_mode (UserNS)
- uts (PodmanArgs --uts)

## not present in compose, present in tetra
- AutoUpdate
- ContainersConfModule
- ExposeHostPort
- GIDMap
- Group (id)
- HttpProxy
- Notify
- PidsLimit
- Pod
- ReloadCmd
- ReloadSignal
- Retry
- RetryDelay
- StartWithPod
- SubGIDMap
- SubUIDMap
- Timezone
- UIDMap

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
