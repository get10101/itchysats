declare module "*.svg" {
    export const ReactComponent: React.FC<React.SVGProps<SVGSVGElement>>;

    const content: string;
    export default content;
}
