import * as cdk from "aws-cdk-lib";
import { Match, Template } from "aws-cdk-lib/assertions";
import { ScreenMemoryStack } from "../lib/infra-stack";

let template: Template;

beforeAll(() => {
  // Skip Lambda asset bundling (Docker) in tests
  const app = new cdk.App({ context: { "aws:cdk:bundling-stacks": [] } });
  const stack = new ScreenMemoryStack(app, "TestStack", {
    env: { account: "123456789012", region: "ap-northeast-1" },
  });
  template = Template.fromStack(stack);
});

test("バケットの raw/ に14日のライフサイクルがある", () => {
  template.hasResourceProperties("AWS::S3::Bucket", {
    LifecycleConfiguration: {
      Rules: [
        Match.objectLike({
          Prefix: "raw/",
          ExpirationInDays: 14,
          Status: "Enabled",
        }),
      ],
    },
  });
});

test("S3 通知は raw/ プレフィックスと .json サフィックスで絞られている", () => {
  template.hasResourceProperties("Custom::S3BucketNotifications", {
    NotificationConfiguration: Match.objectLike({
      LambdaFunctionConfigurations: Match.arrayWith([
        Match.objectLike({
          Events: Match.arrayWith(["s3:ObjectCreated:*"]),
          Filter: Match.objectLike({
            Key: Match.objectLike({
              // arrayWith matches an ordered subsequence, so follow CDK's output order (suffix then prefix)
              FilterRules: Match.arrayWith([
                Match.objectLike({ Name: "suffix", Value: ".json" }),
                Match.objectLike({ Name: "prefix", Value: "raw/" }),
              ]),
            }),
          }),
        }),
      ]),
    }),
  });
});

test("日報スケジュールは JST の 23:55", () => {
  template.hasResourceProperties("AWS::Scheduler::Schedule", {
    Name: "screen-memory-daily-report",
    ScheduleExpression: "cron(55 23 * * ? *)",
    ScheduleExpressionTimezone: "Asia/Tokyo",
  });
});

test("ID プールは未認証アイデンティティを許可しない", () => {
  template.hasResourceProperties("AWS::Cognito::IdentityPool", {
    IdentityPoolName: "screen-memory-identity",
    AllowUnauthenticatedIdentities: false,
  });
});

test("HTTP API の CORS は CloudFront ドメインだけを許可する", () => {
  template.hasResourceProperties("AWS::ApiGatewayV2::Api", {
    CorsConfiguration: {
      AllowHeaders: ["authorization"],
      AllowMethods: ["GET"],
      // A single reference to the CloudFront domain, not a static value such as "*"
      AllowOrigins: [
        {
          "Fn::Join": [
            "",
            [
              "https://",
              { "Fn::GetAtt": [Match.stringLikeRegexp("WebDistribution"), "DomainName"] },
            ],
          ],
        },
      ],
    },
  });
});

test("API の2ルートとも JWT オーソライザーが付く", () => {
  const routes = template.findResources("AWS::ApiGatewayV2::Route");
  const apiRoutes = Object.values(routes).filter((r) =>
    String(r.Properties.RouteKey).startsWith("GET /days/"),
  );
  expect(apiRoutes).toHaveLength(2);
  for (const route of apiRoutes) {
    expect(route.Properties.AuthorizationType).toBe("JWT");
    expect(route.Properties.AuthorizerId).toBeDefined();
  }
});

test("アプリクライアントは認可コードグラントで、コールバックに Web と localhost を持つ", () => {
  template.hasResourceProperties("AWS::Cognito::UserPoolClient", {
    AllowedOAuthFlows: ["code"],
    AllowedOAuthScopes: ["openid"],
    CallbackURLs: Match.arrayWith(["http://localhost:8765/callback"]),
  });
});

test("Cognito ドメインが定義されている", () => {
  template.hasResourceProperties("AWS::Cognito::UserPoolDomain", {
    Domain: "screen-memory-123456789012",
  });
});

test("CloudFront のルートオブジェクトは index.html", () => {
  template.hasResourceProperties("AWS::CloudFront::Distribution", {
    DistributionConfig: Match.objectLike({
      DefaultRootObject: "index.html",
    }),
  });
});

test("認証済みロールのポリシーは raw/* への PutObject のみ", () => {
  template.hasResourceProperties("AWS::IAM::Policy", {
    PolicyDocument: {
      Statement: [
        {
          Action: "s3:PutObject",
          Effect: "Allow",
          Resource: {
            "Fn::Join": Match.arrayWith([
              Match.arrayWith([Match.stringLikeRegexp("/raw/\\*$")]),
            ]),
          },
        },
      ],
    },
    Roles: [{ Ref: Match.stringLikeRegexp("UploaderAuthenticatedRole") }],
  });
});
