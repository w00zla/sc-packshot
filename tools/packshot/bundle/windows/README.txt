packshot bundle (sc-quartermaster), Windows

Contents: packshot.exe, starbreaker\ (StarBreaker CLI, our fork), the official Blender zip, portable\ (Blender
config and StarBreaker add-on), cli\render.py (Blender script), targets.json (target list from the development DB),
setup.cmd, run.cmd, packshot.example.json (cluster settings). The first run start (Enter) asks for live's target list:
Enter takes the targets.json next to it; every set keeps its own (t in view 2 replaces it after a game update).

1. setup.cmd                    unpacks Blender into .\blender and puts portable\ into it (again after a new bundle)
2. For --export: put the game's Data.p4k and its build_manifest.id (both from ...\StarCitizen\LIVE\) next to
   run.cmd, or pass --p4k "<game folder>\Data.p4k"; without build_manifest.id next to Data.p4k --export does not
   start.
3. run.cmd --render --export   starts the TUI with what this node can do now (views: 1 Run, 2 Sets, 3 Nodes; Tab
                               switches): --render (Blender), --export (Data.p4k), both, or neither (then it only
                               starts runs, stores them and watches); + / - and < / > change the counts
   run.cmd --render --export --plain   starts a run here without the TUI
   run.cmd --render --serve    no TUI: works for the other nodes' runs and prints lines
   run.cmd --report            evaluates the latest run
   run.cmd --help              all options
4. Result: every run is a folder runs\<date>_<time>_<name>\ on its head node (its images, run.json, events.jsonl);
   all icons are in sets\live\ (hard links to the runs' images). work\ is only a cache and can go afterwards.
   run.cmd --set <name> renders into a set of its own (branches off live the first time).

Cluster: copy packshot.example.json to packshot.json and set the same token on every node. The nodes find each other
by UDP broadcast on port 47950 and connect by TCP on the same port; nodes the broadcast does not reach go into
"peers" (host names or IPs, port optional). Every node can start a run, here or on another node (key n in view 1),
which then holds it (head node). On the first start Windows asks whether packshot.exe may accept connections: allow
it for private networks (TCP and UDP 47950).

Nothing needs to be installed and no internet is needed: Blender and StarBreaker are in the bundle, setup.cmd only
unpacks (with the tar built into Windows 10 and later).
