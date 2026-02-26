// Auto-resolve library typings for GP2F

export interface PolicyNode {
    kind: string;
    path?: string;
    value?: string;
    children?: PolicyNode[];
}

export interface ActivityConfig {
    policy: PolicyNode;
}

export class JsGp2FServer {
    constructor(config: { port: number });
    register(workflow: JsWorkflow): void;
    start(): Promise<void>;
}

export class JsWorkflow {
    constructor(id: string);
    addActivity(
        name: string,
        config: ActivityConfig,
        handler: (ctx: any) => Promise<void>
    ): void;
    activityCount(): number;
    id(): string;
}
