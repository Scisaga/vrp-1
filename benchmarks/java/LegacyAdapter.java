// Development adapter only. Compiles against the pinned vrp-0 domain and solver.
// Reads generated engine ProtoJSON; never calls the old REST/map preparation path.
import com.fasterxml.jackson.databind.*;
import com.fasterxml.jackson.databind.node.*;
import java.nio.*;
import java.nio.file.*;
import java.time.*;
import java.util.*;
import java.io.*;
import java.lang.management.*;
import one.rewind.xforce.geo.POI;
import one.rewind.xforce.geo.transit.*;
import one.rewind.xforce.vehicle_routing.domain.RoutePlan;
import one.rewind.xforce.vehicle_routing.domain.agent.*;
import one.rewind.xforce.vehicle_routing.domain.ticket.Ticket;
import one.rewind.xforce.vehicle_routing.domain.cost.CostParameter;
import one.rewind.xforce.vehicle_routing.solver.*;
import org.optaplanner.core.api.solver.*;
import org.optaplanner.core.api.score.buildin.hardmediumsoftlong.HardMediumSoftLongScore;
import org.optaplanner.core.config.solver.SolverConfig;
import org.optaplanner.core.config.phase.custom.CustomPhaseConfig;
import org.optaplanner.core.config.constructionheuristic.ConstructionHeuristicPhaseConfig;

public class LegacyAdapter {
    static final ObjectMapper JSON = new ObjectMapper();
    static final LocalDateTime ZERO = LocalDateTime.of(2026,1,2,0,0);
    static long number(JsonNode n, String k) { return n.path(k).asLong(); }
    static List<String> strings(JsonNode n) {
        List<String> out = new ArrayList<>(); n.forEach(x -> out.add(x.asText())); return out;
    }
    static Map<String,Double> levels(JsonNode n) {
        Map<String,Double> out = new HashMap<>(); n.fields().forEachRemaining(e -> out.put(e.getKey(),e.getValue().asDouble())); return out;
    }
    static long[] score(HardMediumSoftLongScore s) { return new long[]{s.hardScore(),s.mediumScore(),s.softScore()}; }
    static RoutePlan map(JsonNode r) throws Exception {
        JsonNode p = r.path("problem");
        if (p.has("execution") || p.path("capacityDimensions").size() > 1 || p.path("precedences").size()>0)
            throw new IllegalArgumentException("unsupported legacy semantics");
        POI[] pois = new POI[p.path("locations").size()];
        for(int i=0;i<pois.length;i++) pois[i] = new POI(p.path("locations").get(i).path("id").asText());
        TransitMatrix matrix = new TransitMatrix();
        JsonNode m = p.path("matrices").get(0);
        ByteBuffer times = ByteBuffer.wrap(Base64.getDecoder().decode(m.path("durations").asText())).order(ByteOrder.LITTLE_ENDIAN);
        ByteBuffer distances = ByteBuffer.wrap(Base64.getDecoder().decode(m.path("distances").asText())).order(ByteOrder.LITTLE_ENDIAN);
        for(POI a:pois) for(POI b:pois) {
            long t = Integer.toUnsignedLong(times.getInt()), d = Integer.toUnsignedLong(distances.getInt());
            if(t>=0xfffffffeL || d>=0xfffffffeL || t%60!=0) throw new IllegalArgumentException("common matrix must be complete and minute-aligned");
            matrix.put(a.id,b.id,new Transit(d,t));
        }
        List<AgentEachDay> agents = new ArrayList<>();
        for(JsonNode a:p.path("agents")) {
            if(number(a,"startLocation")!=number(a,"endLocation") || number(a,"matrix")!=0) throw new IllegalArgumentException("legacy requires round trips / one matrix");
            AgentEachDay v = new AgentEachDay();
            v.setId(a.path("id").asText()); v.setName(v.getId()); v.setDate(ZERO.toLocalDate());
            v.setStartLoc(pois[(int)number(a,"startLocation")]);
            v.setShiftStartTime(ZERO.plusSeconds(number(a,"startTime")));
            v.setShiftOffTime(ZERO.plusSeconds(a.path("endTime").asLong(86340)));
            v.setWeight(a.path("capacity").path(0).asDouble()); v.setVol(0);
            v.setSkills(strings(a.path("skills"))); v.setQualificationLevels(levels(a.path("qualificationLevels")));
            if(v.getSkills().isEmpty()) throw new IllegalArgumentException("empty skill lists have different legacy semantics");
            v.setDepoId(a.path("depotId").asText()); v.setMaxTicketNum((int)number(a,"maxTickets"));
            v.setTickets(new ArrayList<>()); agents.add(v);
        }
        int realCount = agents.size();
        AgentEachDay virtual = new AgentEachDay();
        virtual.setId("BENCH-UNASSIGNED"); virtual.setName("BENCH-UNASSIGNED"); virtual.setVirtual(true);
        virtual.setTickets(new ArrayList<>()); virtual.setStartLoc(pois[0]);
        virtual.setShiftStartTime(ZERO); virtual.setShiftOffTime(ZERO.plusDays(1)); agents.add(virtual);
        List<Ticket> tickets = new ArrayList<>();
        for(JsonNode t:p.path("tickets")) {
            Ticket q = new Ticket(); q.setId(t.path("id").asText());
            q.setType(t.path("type").asText().endsWith("DELIVERY") || number(t,"type")==1 ? Ticket.Type.Delv : Ticket.Type.Inst);
            q.setLoc(pois[(int)number(t,"location")]); q.setDuration(Duration.ofSeconds(number(t,"duration")));
            q.setMinStartTime(ZERO.plusSeconds(number(t,"earliestStart")));
            q.setMaxEndTime(ZERO.plusSeconds(number(t,"latestStart")));
            q.setSkillsRequired(strings(t.path("skillsRequired"))); q.setQualificationLevelsRequired(levels(t.path("qualificationRequired")));
            q.setDepoId(t.path("depotId").asText()); q.setWeight((float)t.path("demand").path(0).asDouble()); q.setVol(0);
            q.setDepTickets(new ArrayList<>()); q.setRefTickets(new ArrayList<>()); tickets.add(q);
        }
        RoutePlan plan = new RoutePlan(); plan.setAgents(agents); plan.setTickets(tickets); plan.setMatrix(matrix);
        CostParameter cost = new CostParameter(); cost.setMaxOverloadRatio(1.0);
        // The constructor binds only facts. No import normalization or time-window rewriting.
        plan = new RoutePlan(new ArrayList<>(),new ArrayList<>(),agents,tickets,matrix,new RoutePlanConstraintConfiguration(),cost);
        RoutePlanConstraintConfiguration weights = new RoutePlanConstraintConfiguration();
        // Start with all old constraints OFF; require an explicit weight for every one.
        for(var method:weights.getClass().getMethods()) if(method.getName().startsWith("set") && method.getParameterCount()==1 && method.getParameterTypes()[0]==HardMediumSoftLongScore.class)
            method.invoke(weights,HardMediumSoftLongScore.ZERO);
        var fields = p.path("weights").fields();
        while(fields.hasNext()) {
            var e = fields.next(); String key=e.getKey().equals("ticketUnassigned")?"agentIsVirtual":e.getKey();
            HardMediumSoftLongScore w = HardMediumSoftLongScore.of(number(e.getValue(),"hard"),number(e.getValue(),"medium"),number(e.getValue(),"soft"));
            String setter = "set"+Character.toUpperCase(key.charAt(0))+key.substring(1);
            try { weights.getClass().getMethod(setter,HardMediumSoftLongScore.class).invoke(weights,w); }
            catch(NoSuchMethodException ex) { if(!w.equals(HardMediumSoftLongScore.ZERO)) throw new IllegalArgumentException("unsupported weight "+key); }
        }
        plan.setConstraintConfiguration(weights);
        if(r.has("initialSolution")) {
            Set<Integer> seen = new HashSet<>();
            for(JsonNode route:r.path("initialSolution").path("routes")) {
                int a=(int)number(route,"agent");
                if(a>=realCount) throw new IllegalArgumentException("bad route");
                for(JsonNode ti:route.path("tickets")) {
                    int t=ti.asInt(); if(!seen.add(t)) throw new IllegalArgumentException("duplicate assignment");
                    agents.get(a).getTickets().add(tickets.get(t));
                }
            }
            for(int i=0;i<tickets.size();i++) if(!seen.contains(i)) virtual.getTickets().add(tickets.get(i));
        }
        // Original-agent penalties remain explicitly zero for the shared continuation start.
        return plan;
    }
    static SolverFactory<RoutePlan> factory(long budget,long seed,int threads) {
        return SolverFactory.create(new SolverConfig().withSolutionClass(RoutePlan.class)
            .withEntityClasses(AgentEachDay.class,Ticket.class).withConstraintProviderClass(RoutePlanConstraintProvider.class)
            .withRandomSeed(seed).withMoveThreadCount(threads==1?"NONE":Integer.toString(threads))
            .withTerminationSpentLimit(Duration.ofMillis(budget))
            .withPhases(new CustomPhaseConfig().withCustomPhaseCommands(new InitialArrivalTimeCustomPhaseCommand()),
                new ConstructionHeuristicPhaseConfig(),SolverWrapper.LocalSearchConfig_1.get()));
    }
    static ObjectNode output(RoutePlan plan,int realCount) {
        ObjectNode result=JSON.createObjectNode(), sol=result.putObject("solution"); ArrayNode routes=sol.putArray("routes");
        Map<Ticket,Integer> idx=new IdentityHashMap<>(); for(int i=0;i<plan.getTickets().size();i++) idx.put(plan.getTickets().get(i),i);
        for(int a=0;a<realCount;a++) {
            ObjectNode route=routes.addObject().put("agent",a); ArrayNode ts=route.putArray("tickets");
            for(Ticket t:plan.getAgents().get(a).getTickets()) ts.add(idx.get(t));
        }
        ArrayNode unassigned=result.putArray("unassigned");
        Set<Ticket> assigned=Collections.newSetFromMap(new IdentityHashMap<>());
        for(int a=0;a<realCount;a++) assigned.addAll(plan.getAgents().get(a).getTickets());
        for(int i=0;i<plan.getTickets().size();i++) if(!assigned.contains(plan.getTickets().get(i))) unassigned.addObject().put("ticket",i);
        ObjectNode s=result.putObject("score"); var sc=plan.getScore(); s.put("hard",sc.hardScore()).put("medium",sc.mediumScore()).put("soft",sc.softScore());
        result.put("legacyInitScore",sc.initScore());
        return result;
    }
    static double ms(long begin) { return (System.nanoTime()-begin)/1e6; }
    static void execute(JsonNode command,boolean quiet) throws Exception {
        var os=(com.sun.management.OperatingSystemMXBean)ManagementFactory.getOperatingSystemMXBean();
        long cpuBegin=os.getProcessCpuTime();
        long begin=System.nanoTime();
        JsonNode request=JSON.readTree(Files.readString(Path.of(command.path("input").asText())));
        RoutePlan plan=map(request); double inputMs=ms(begin); int realCount=plan.getAgents().size()-1;
        // Initial-solution work is charged while factory compilation is separate.
        begin=System.nanoTime(); RouteScheduleInitializer.initializePreassignedRoutes(plan);
        double shadowMs=ms(begin);
        long remainingBudget=Math.max(1,command.path("budget_ms").asLong(30000)-(long)Math.ceil(shadowMs));
        begin=System.nanoTime();
        var factory=factory(remainingBudget,command.path("seed").asLong(),command.path("threads").asInt(1));
        var solver=factory.buildSolver(); double compileMs=ms(begin);
        String eventPath=command.path("events").asText();
        try(PrintWriter events=new PrintWriter(quiet?Writer.nullWriter():Files.newBufferedWriter(Path.of(eventPath)))) {
            events.println(JSON.writeValueAsString(Map.of("event","ready","input_ms",inputMs,"compile_ms",compileMs,"initial_shadow_ms",shadowMs))); events.flush();
            solver.addEventListener(e -> {
                if(!e.getNewBestScore().isSolutionInitialized()) return;
                int assigned=0; for(int a=0;a<realCount;a++) assigned+=e.getNewBestSolution().getAgents().get(a).getTickets().size();
                try { events.println(JSON.writeValueAsString(Map.of("event","incumbent","elapsed_ms",shadowMs+e.getTimeMillisSpent(),"score",score((HardMediumSoftLongScore)e.getNewBestScore()),"assigned",assigned))); events.flush(); }
                catch(Exception ex) { throw new RuntimeException(ex); }
            });
            begin=System.nanoTime();
            RoutePlan best=solver.solve(plan); double solveMs=shadowMs+ms(begin);
            if(!quiet) {
                begin=System.nanoTime();
                Files.writeString(Path.of(command.path("output").asText()),JSON.writeValueAsString(output(best,realCount)));
                events.println(JSON.writeValueAsString(Map.of("event","finished","solve_ms",solveMs,"output_ms",ms(begin),"process_cpu_seconds",(os.getProcessCpuTime()-cpuBegin)/1e9))); events.flush();
            } else {
                System.out.println(JSON.writeValueAsString(Map.of("event","warmup_phase","input_ms",inputMs,"compile_ms",compileMs,"solve_ms",solveMs)));
                System.out.flush();
            }
        }
    }
    public static void main(String[] args) throws Exception {
        org.jboss.logmanager.LogContext.getLogContext().getLogger("").setLevel(java.util.logging.Level.WARNING);
        java.util.logging.Logger.getLogger("").setLevel(java.util.logging.Level.WARNING);
        System.out.println(JSON.writeValueAsString(Map.of("event","process_started","unix_ms",System.currentTimeMillis()))); System.out.flush();
        if(args[0].equals("eval-many")) {
            var f=factory(1,0,1); SolutionManager<RoutePlan,HardMediumSoftLongScore> sm=SolutionManager.create(f);
            for(JsonNode command:JSON.readTree(Files.readString(Path.of(args[1])))) {
                RoutePlan plan=map(JSON.readTree(Files.readString(Path.of(command.path("input").asText()))));
                RouteScheduleInitializer.initializePreassignedRoutes(plan); sm.update(plan);
                Files.writeString(Path.of(command.path("output").asText()),JSON.writeValueAsString(output(plan,plan.getAgents().size()-1)));
            }
            return;
        }
        if(args[0].equals("eval")) {
            JsonNode r=JSON.readTree(Files.readString(Path.of(args[1]))); RoutePlan plan=map(r);
            RouteScheduleInitializer.initializePreassignedRoutes(plan);
            var f=factory(1,0,1); SolutionManager<RoutePlan,HardMediumSoftLongScore> sm=SolutionManager.create(f); sm.update(plan);
            Files.writeString(Path.of(args[2]),JSON.writeValueAsString(output(plan,plan.getAgents().size()-1))); return;
        }
        JsonNode batch=JSON.readTree(Files.readString(Path.of(args[1])));
        long warmBegin=System.nanoTime(); CompilationMXBean jit=ManagementFactory.getCompilationMXBean();
        List<Long> jitDeltas=new ArrayList<>();
        for(int i=0;i<batch.path("warmup_rounds").asInt(3);i++) {
            ObjectNode warm=((ObjectNode)batch.path("runs").get(0)).deepCopy(); warm.put("budget_ms",batch.path("warmup_ms").asLong(20000));
            warm.put("seed",100000+i); long c=jit.getTotalCompilationTime(); execute(warm,true); jitDeltas.add(jit.getTotalCompilationTime()-c);
        }
        System.out.println(JSON.writeValueAsString(Map.of("event","warmed","wall_ms",ms(warmBegin),"jit_compilation_ms",jitDeltas))); System.out.flush();
        for(JsonNode command:batch.path("runs")) {
            System.out.println(JSON.writeValueAsString(Map.of("event","run_start","id",command.path("id").asText()))); System.out.flush();
            execute(command,false);
            System.out.println(JSON.writeValueAsString(Map.of("event","run_end","id",command.path("id").asText()))); System.out.flush();
        }
    }
}
